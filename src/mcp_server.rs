// ── Módulo MCP Server ──────────────────────────────────────────────────────
//
// Servidor Model Context Protocol (MCP) sobre stdio.
// Comunica via JSON-RPC 2.0, expondo Resources (leitura) e Tools (ações)
// do domínio AdvWiki.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::embeddings::{EmbeddingProvider, SemanticConfig};
use crate::search::{DocumentKind, WikiSearchEngine};
use crate::storage::WikiFileManager;
use crate::vector_store::{RRF_K, SemanticHit, VectorStore, W_BM25, W_SEM, rrf_fuse};

// tipos JSON-RPC

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

impl JsonRpcRequest {
    fn is_jsonrpc_2_0(&self) -> bool {
        self.jsonrpc == "2.0"
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// tipos mcp
// nomes em camelCase seguem a especificação json do mcp.

#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
struct McpInitializeResult {
    protocolVersion: String,
    capabilities: Value,
    serverInfo: Value,
}

#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
struct McpResource {
    uri: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mimeType: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
struct McpResourceContent {
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mimeType: Option<String>,
    text: String,
}

#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
struct McpTool {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    inputSchema: Value,
}

#[derive(Debug, Serialize)]
struct McpToolContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

// negociação de Protocolo

/// versões de protocolo mcp suportadas, em ordem de preferência (mais recente primeiro).
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// negocia a versão do protocolo mcp com o cliente.
///
/// se o cliente solicitar uma versão suportada, ela é aceita.
/// caso contrário, retorna a versão mais recente suportada.
fn negotiate_protocol(requested: Option<&str>) -> &str {
    match requested {
        Some(v) if SUPPORTED_PROTOCOLS.contains(&v) => v,
        _ => SUPPORTED_PROTOCOLS[0],
    }
}

fn guess_mime_from_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "md" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "xml" => "application/xml",
        _ => return None,
    };
    Some(mime.to_string())
}

// servidor MCP

pub struct AdvWikiMcpServer {
    file_manager: Arc<WikiFileManager>,
    search_engine: Arc<WikiSearchEngine>,
    // Busca semântica (opt-in). `None` quando `DD_WIKI_OPENAI_APIKEY` ausente.
    // Quando presentes, `query_wiki` funde BM25 + semântica e `lint_wiki` reporta
    // o status. Os três andam juntos (todos `Some` ou todos `None`).
    vector_store: Option<Arc<VectorStore>>,
    embed_provider: Option<Arc<dyn EmbeddingProvider>>,
    semantic_cfg: Option<SemanticConfig>,
}

impl AdvWikiMcpServer {
    pub fn new(
        file_manager: Arc<WikiFileManager>,
        search_engine: Arc<WikiSearchEngine>,
        vector_store: Option<Arc<VectorStore>>,
        embed_provider: Option<Arc<dyn EmbeddingProvider>>,
        semantic_cfg: Option<SemanticConfig>,
    ) -> Self {
        Self {
            file_manager,
            search_engine,
            vector_store,
            embed_provider,
            semantic_cfg,
        }
    }

    fn build_source_id(source_uri: &str) -> String {
        use md5::Digest;
        let digest = md5::Md5::digest(source_uri.as_bytes());
        digest.iter().map(|b| format!("{:02x}", b)).collect()
    }

    async fn load_source_content(source_uri: &str) -> Result<(Vec<u8>, Option<String>), String> {
        if source_uri.starts_with("http://") || source_uri.starts_with("https://") {
            let response = reqwest::get(source_uri)
                .await
                .map_err(|e| format!("HTTP request error: {e}"))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP request failed with status {status}"));
            }
            let mime_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
            let bytes = response
                .bytes()
                .await
                .map_err(|e| format!("HTTP body read error: {e}"))?;
            return Ok((bytes.to_vec(), mime_type));
        }

        let path = Path::new(source_uri);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| format!("File read error: {e}"))?;
        let mime_type = guess_mime_from_path(path);
        Ok((bytes, mime_type))
    }

    /// inicia o loop principal do servidor mcp sobre stdin/stdout.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("Servidor MCP iniciado — aguardando requisições em stdin");

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        let mut stdout = tokio::io::stdout();

        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(req) => {
                    let id = req.id.clone();
                    let response = if req.is_jsonrpc_2_0() {
                        self.handle_request(req).await
                    } else {
                        JsonRpcResponse::error(id.clone(), -32600, "Invalid Request: jsonrpc must be '2.0'")
                    };
                    // Notifications (id: None) não recebem resposta
                    if id.is_some() {
                        let mut response_str = serde_json::to_string(&response)?;
                        response_str.push('\n');
                        stdout.write_all(response_str.as_bytes()).await?;
                        stdout.flush().await?;
                    }
                }
                Err(e) => {
                    let response = JsonRpcResponse::error(None, -32700, &format!("Parse error: {e}"));
                    let mut response_str = serde_json::to_string(&response)?;
                    response_str.push('\n');
                    stdout.write_all(response_str.as_bytes()).await?;
                    stdout.flush().await?;
                }
            }
        }

        tracing::info!("Servidor MCP encerrado");
        Ok(())
    }

    /// roteia uma requisição JSON-RPC para o handler apropriado.
    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id, req.params).await,
            "resources/list" => self.handle_list_resources(req.id).await,
            "resources/read" => self.handle_read_resource(req.id, req.params).await,
            "tools/list" => self.handle_list_tools(req.id).await,
            "tools/call" => self.handle_call_tool(req.id, req.params).await,
            _ => JsonRpcResponse::error(
                req.id,
                -32601,
                &format!("Method not found: {}", req.method),
            ),
        }
    }

    // initialize

    async fn handle_initialize(
        &self,
        id: Option<Value>,
        params: Option<Value>,
    ) -> JsonRpcResponse {
        let requested = params
            .as_ref()
            .and_then(|p| p.get("protocolVersion"))
            .and_then(|v| v.as_str());

        let result = McpInitializeResult {
            protocolVersion: negotiate_protocol(requested).into(),
            capabilities: json!({
                "resources": {
                    "listChanged": false
                },
                "tools": {
                    "listChanged": false
                }
            }),
            serverInfo: json!({
                "name": "advwiki-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }),
        };

        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    // resources

    async fn handle_list_resources(&self, id: Option<Value>) -> JsonRpcResponse {
        let mut resources = Vec::new();

        // Recursos fixos
        resources.push(McpResource {
            uri: "wiki://log".into(),
            name: "Operational Log".into(),
            description: Some("Wiki operations log".into()),
            mimeType: Some("text/markdown".into()),
        });

        resources.push(McpResource {
            uri: "wiki://index".into(),
            name: "Main Index".into(),
            description: Some("Raw source index (rawindex.md)".into()),
            mimeType: Some("text/markdown".into()),
        });

        // paginas dinamicas
        match self.file_manager.list_pages().await {
            Ok(slugs) => {
                for slug in slugs {
                    resources.push(McpResource {
                        uri: format!("wiki://page/{slug}"),
                        name: slug.replace('-', " "),
                        description: Some(format!("Wiki page: {}", slug)),
                        mimeType: Some("text/markdown".into()),
                    });
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Falha ao listar páginas para resources");
            }
        }

        // raw resources
        match self.file_manager.list_raw_sources().await {
            Ok(ids) => {
                for source_id in ids {
                    resources.push(McpResource {
                        uri: format!("raw://source/{source_id}"),
                        name: format!("Raw Source: {source_id}"),
                        description: Some(format!("Raw content: {source_id}")),
                        mimeType: Some("text/plain".into()),
                    });
                    resources.push(McpResource {
                        uri: format!("raw://sourcemetadata/{source_id}"),
                        name: format!("Metadata: {source_id}"),
                        description: Some(format!("Source JSON metadata: {source_id}")),
                        mimeType: Some("application/json".into()),
                    });
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Falha ao listar raw sources para resources");
            }
        }

        JsonRpcResponse::success(
            id,
            json!({ "resources": resources }),
        )
    }

    async fn handle_read_resource(
        &self,
        id: Option<Value>,
        params: Option<Value>,
    ) -> JsonRpcResponse {
        let uri = match params
            .as_ref()
            .and_then(|p| p.get("uri"))
            .and_then(|v| v.as_str())
        {
            Some(uri) => uri,
            None => {
                return JsonRpcResponse::error(id, -32602, "Missing required param: uri");
            }
        };

        let (content, mime_type) = match self.read_resource_by_uri(uri).await {
            Ok((text, mime)) => (text, mime),
            Err(e) => {
                return JsonRpcResponse::error(id, -32000, &format!("Resource read error: {e}"));
            }
        };

        let resource_content = McpResourceContent {
            uri: uri.into(),
            mimeType: Some(mime_type),
            text: content,
        };

        JsonRpcResponse::success(
            id,
            json!({ "contents": [resource_content] }),
        )
    }

    async fn read_resource_by_uri(&self, uri: &str) -> anyhow::Result<(String, String)> {
        if uri == "wiki://log" {
            let content = self.file_manager.read_log().await?;
            return Ok((content, "text/markdown".into()));
        }

        if uri == "wiki://index" || uri == "wiki://rawindex" {
            let entries = self.file_manager.read_raw_index().await?;
            let text = entries
                .iter()
                .map(|e| format!("{} | {} | {}", e.source_id, e.original_path.as_deref().unwrap_or("-"), e.extracted_at))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok((text, "text/markdown".into()));
        }

        if let Some(slug) = uri.strip_prefix("wiki://page/") {
            let content = self.file_manager.read_page(slug).await?;
            return Ok((content, "text/markdown".into()));
        }

        if let Some(source_id) = uri.strip_prefix("raw://source/") {
            let content = self.file_manager.read_raw_source(source_id).await?;
            return Ok((content, "text/plain".into()));
        }

        if let Some(source_id) = uri.strip_prefix("raw://sourcemetadata/") {
            let meta = self.file_manager.read_raw_source_metadata(source_id).await?;
            let json = serde_json::to_string_pretty(&meta)?;
            return Ok((json, "application/json".into()));
        }

        anyhow::bail!("Recurso não encontrado: {uri}");
    }

    // tools

    async fn handle_list_tools(&self, id: Option<Value>) -> JsonRpcResponse {
        let tools = vec![
            McpTool {
                name: "query_wiki".into(),
                description: Some("Search the wiki. Hybrid by default (lexical BM25 + semantic) when semantic search is enabled on the server; otherwise pure BM25. Prefer short queries with exact terms — service names, modules, technologies, tables, queues, endpoints, classes, config keys, error strings. Returns the matching pages and raw sources.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "Search terms (e.g. 'rust memory safety')"
                        },
                        "includeRawReferences": {
                            "type": "boolean",
                            "description": "If true, include raw sources in the results (always via BM25). Worth it for audits, source checking and traceability; noise otherwise.",
                            "default": false
                        },
                        "maxPages": {
                            "type": "integer",
                            "description": "Maximum number of results",
                            "default": 10,
                            "minimum": 1,
                            "maximum": 50
                        },
                        "mode": {
                            "type": "string",
                            "description": "Search strategy: 'auto' fuses BM25 and semantic (recommended); 'bm25' forces lexical only; 'semantic' prefers meaning (falls back to BM25 when semantic is unavailable). No effect when semantic search is disabled. Only pages are embedded — raw sources are always BM25.",
                            "enum": ["auto", "bm25", "semantic"],
                            "default": "auto"
                        }
                    },
                    "required": ["question"]
                }),
            },
            McpTool {
                name: "update_page".into(),
                description: Some("Create or update a wiki page. Without 'section', 'content' is the whole document ('overwrite' replaces everything, 'append' adds to the end). With 'section', the operation affects ONLY that heading: 'overwrite' replaces its body, 'append' adds to its end — preferred for editing one part without resending the whole page. Body links must use wikilink syntax: [[slug]] or [[slug|Display text]].".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Unique page identifier (e.g. 'getting-started'). ASCII [a-zA-Z0-9._-] only — no spaces, no accents, no /\\:. Cannot start or end with '.' nor end with a space. Windows reserved names (CON, PRN, AUX, NUL, COM1-9, LPT1-9) are rejected."
                        },
                        "mode": {
                            "type": "string",
                            "description": "Write mode: 'overwrite' (replace) or 'append' (add to the end). With 'section', the scope is the section, not the document.",
                            "enum": ["overwrite", "append"]
                        },
                        "content": {
                            "type": "string",
                            "description": "Markdown content. Without 'section', the whole page; with 'section', only that section's body. 'created_at'/'updated_at' are written by the server on every save — never set them by hand."
                        },
                        "section": {
                            "type": "string",
                            "description": "Optional. Title of an existing ATX heading (e.g. 'Details' or '## Details'), matched case-insensitively, with or without the '#'. Only that section changes and the rest of the page is rebuilt byte-for-byte; editing '## X' includes its '### Y' subsections. The page must exist, and an unknown or duplicated title errors out without writing — there is no silent fallback."
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Reason for the change (recorded in the operational log)"
                        }
                    },
                    "required": ["slug", "mode", "content"]
                }),
            },
            McpTool {
                name: "set_page_metadata".into(),
                description: Some("Edit an EXISTING page's frontmatter without resending the body. 'set' assigns scalar fields (e.g. type, project, status, confidence, owner); 'add'/'remove' add or drop items of list fields (e.g. tags, related, sources) without duplicating. Fields not mentioned — including unknown/custom ones — are preserved. 'created_at'/'updated_at' are server-managed and cannot be set.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Page slug (also accepts the wiki://page/{slug} URI)"
                        },
                        "set": {
                            "type": "object",
                            "description": "Scalar fields to set/replace, e.g. {\"status\": \"active\", \"project\": \"auth\"}",
                            "additionalProperties": { "type": "string" }
                        },
                        "add": {
                            "type": "object",
                            "description": "Items to add to list fields (no duplicates), e.g. {\"tags\": [\"backend\"], \"related\": [\"other-page\"]}",
                            "additionalProperties": { "type": "array", "items": { "type": "string" } }
                        },
                        "remove": {
                            "type": "object",
                            "description": "Items to remove from list fields, e.g. {\"tags\": [\"obsolete\"]}",
                            "additionalProperties": { "type": "array", "items": { "type": "string" } }
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Reason for the change (recorded in the operational log)"
                        }
                    },
                    "required": ["slug"]
                }),
            },
            McpTool {
                name: "ingest_source".into(),
                description: Some("Ingest an external file as a raw source. The source_id is the MD5 of the URI, so it is stable: re-ingesting the same URI needs force: true.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "sourceUri": {
                            "type": "string",
                            "description": "URI of the external file to ingest"
                        },
                        "sourceType": {
                            "type": "string",
                            "description": "Content type (e.g. 'pdf', 'markdown', 'text')"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "If true, overwrite an existing source with the same ID",
                            "default": false
                        }
                    },
                    "required": ["sourceUri", "sourceType"]
                }),
            },
            McpTool {
                name: "ingest_extracted_content".into(),
                description: Some("Store already-extracted text directly as a raw source. This is where logs, stack traces, pasted specs, error output, tool output and long messages belong — keep them out of curated pages and reference them from there. The source_id is the MD5 of the logical URI: re-ingesting the same URI needs force: true.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "logicalUri": {
                            "type": "string",
                            "description": "Target logical URI (e.g. 'raw://source/my-doc')"
                        },
                        "sourceType": {
                            "type": "string",
                            "description": "Type of the original content (e.g. 'pdf', 'markdown')"
                        },
                        "title": {
                            "type": "string",
                            "description": "Descriptive title for the content"
                        },
                        "content": {
                            "type": "string",
                            "description": "Extracted text content"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "If true, overwrite an existing source",
                            "default": false
                        }
                    },
                    "required": ["logicalUri", "sourceType", "title", "content"]
                }),
            },
            McpTool {
                name: "lint_wiki".into(),
                description: Some("Run structural validation over the wiki and return a report. It also reports a Semantic search status line: enabled or disabled, the model, and embedding coverage as N/M pages.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Validation scope: 'quick' for everyday hygiene; 'all' for a full audit, which additionally reports stale pages, decisions without rationale and near-duplicates.",
                            "enum": ["all", "quick"]
                        }
                    },
                    "required": ["scope"]
                }),
            },
            McpTool {
                name: "read_knowledge_uri".into(),
                description: Some("Read the content of any wiki logical URI: wiki://page/{slug}, wiki://log, wiki://index, wiki://rawindex, raw://source/{id}, raw://sourcemetadata/{id}. Note that wiki://list and raw://sources are NOT routed — enumerate with resources/list or the list_pages_by_* tools instead.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "uri": {
                            "type": "string",
                            "description": "Logical URI to read (e.g. wiki://page/home, raw://source/abc, wiki://log)"
                        }
                    },
                    "required": ["uri"]
                }),
            },
            McpTool {
                name: "delete_page".into(),
                description: Some("Delete a wiki page by slug.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Slug of the page to delete"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Reason for the removal (recorded in the operational log)"
                        }
                    },
                    "required": ["slug"]
                }),
            },
            McpTool {
                name: "delete_raw_source".into(),
                description: Some("Delete a raw source (raw content + metadata) and update the rawindex.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "sourceId": {
                            "type": "string",
                            "description": "Identifier of the raw source to delete"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Reason for the removal (recorded in the operational log)"
                        }
                    },
                    "required": ["sourceId"]
                }),
            },
            McpTool {
                name: "list_pages_by_type".into(),
                description: Some("List wiki pages whose frontmatter 'type' equals the given value.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "pageType": {
                            "type": "string",
                            "description": "Value of the 'type' field to filter by (e.g. 'service', 'decision', 'pattern', 'bug', 'runbook')"
                        }
                    },
                    "required": ["pageType"]
                }),
            },
            McpTool {
                name: "list_pages_by_project".into(),
                description: Some("List wiki pages whose frontmatter 'project' equals the given value.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Project name to filter by (e.g. 'auth-service', 'gateway')"
                        }
                    },
                    "required": ["project"]
                }),
            },
            McpTool {
                name: "list_pages_by_tag".into(),
                description: Some("List wiki pages carrying the given tag in their frontmatter.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "tag": {
                            "type": "string",
                            "description": "Tag to filter by (e.g. 'backend', 'api', 'mcp')"
                        }
                    },
                    "required": ["tag"]
                }),
            },
            McpTool {
                name: "find_pages_without_sources".into(),
                description: Some("List wiki pages with no 'sources' field in the frontmatter (or an empty one) — candidates for review or for linking to raw sources.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "propose_page_update".into(),
                description: Some("Propose a page change without writing it: stores a reviewable proposal and returns the diff between current and proposed content. Show the diff to the user, then apply it with apply_page_update. Prefer this over a direct update_page when the change is substantial or risky. With 'section', 'content' is only that section's new body (the server rebuilds the document), which keeps the diff small and focused.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Identifier of the target page (e.g. 'getting-started')"
                        },
                        "content": {
                            "type": "string",
                            "description": "Without 'section': the full proposed Markdown for the page. With 'section': only that section's new body."
                        },
                        "section": {
                            "type": "string",
                            "description": "Optional. Title of an existing heading (e.g. 'Details'). The proposal then replaces only that section and preserves the rest. The page must exist; an unknown or ambiguous title errors out."
                        },
                        "reason": {
                            "type": "string",
                            "description": "Reason for the change (recorded in the proposal and in the log when applied)"
                        }
                    },
                    "required": ["slug", "content", "reason"]
                }),
            },
            McpTool {
                name: "apply_page_update".into(),
                description: Some("Apply a proposal created by propose_page_update, normally after the user approves the diff. Re-checks by MD5 that the page has not changed since the proposal and refuses if it has.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "proposalId": {
                            "type": "string",
                            "description": "Proposal ID returned by propose_page_update"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "If true, apply even when the page changed since the proposal",
                            "default": false
                        }
                    },
                    "required": ["proposalId"]
                }),
            },
            McpTool {
                name: "wiki_graph".into(),
                description: Some("Return the wiki link graph (nodes, edges, hubs, orphans and broken links). Edges come from body wikilinks — [[slug]], plus the legacy wiki://page/ form still accepted for reading — and from the frontmatter 'related' field.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "format": {
                            "type": "string",
                            "description": "Output format: 'summary' (overview + hubs), 'full' (adjacency list) or 'mermaid' (diagram)",
                            "enum": ["summary", "full", "mermaid"],
                            "default": "summary"
                        }
                    }
                }),
            },
            McpTool {
                name: "backlinks".into(),
                description: Some("List the pages that point to a page (backlinks).".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Page slug (also accepts the wiki://page/{slug} URI)"
                        }
                    },
                    "required": ["slug"]
                }),
            },
            McpTool {
                name: "orphans".into(),
                description: Some("List orphan pages — no other page points to them. Links from the generated 'index' page are ignored.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "related_pages".into(),
                description: Some("List pages related to a page, classifying the relation (bidirectional, declared in 'related', points to, pointed to by).".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Page slug (also accepts the wiki://page/{slug} URI)"
                        }
                    },
                    "required": ["slug"]
                }),
            },
            McpTool {
                name: "link_suggestions".into(),
                description: Some("Suggest links between pages that are not connected yet, combining content similarity with shared project and tags.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "If given, only suggest links involving this page. Without it, scans the whole wiki."
                        },
                        "maxSuggestions": {
                            "type": "integer",
                            "description": "Maximum number of suggestions",
                            "default": 10,
                            "minimum": 1,
                            "maximum": 50
                        },
                        "minSimilarity": {
                            "type": "number",
                            "description": "Minimum score for a suggestion to show up (0.0 to 1.0)",
                            "default": 0.15
                        }
                    }
                }),
            },
            McpTool {
                name: "find_claims".into(),
                description: Some("List the traceable claims (the `## Claims` block) across wiki pages, with text, source, confidence and verification date.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "If given, list only this page's claims. Without it, scans the whole wiki."
                        }
                    }
                }),
            },
            McpTool {
                name: "find_claims_without_source".into(),
                description: Some("List claims missing the 'Source' field — statements with no documented origin.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "find_conflicting_claims".into(),
                description: Some("Heuristic: flags pairs of claims with overlapping vocabulary as conflict-review candidates. It does not detect actual contradiction — it only points at pairs worth reviewing.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "minSimilarity": {
                            "type": "number",
                            "description": "Minimum term overlap (Jaccard, 0.0 to 1.0) for a pair to show up",
                            "default": 0.25
                        }
                    }
                }),
            },
            McpTool {
                name: "verify_claim".into(),
                description: Some("Update a claim's 'Last verified' date, marking it as verified.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Page slug (also accepts the wiki://page/{slug} URI)"
                        },
                        "claimIndex": {
                            "type": "integer",
                            "description": "1-based index of the claim within the `## Claims` block (use find_claims to see them)",
                            "minimum": 1
                        },
                        "date": {
                            "type": "string",
                            "description": "Verification date (YYYY-MM-DD). Default: today."
                        }
                    },
                    "required": ["slug", "claimIndex"]
                }),
            },
        ];

        JsonRpcResponse::success(
            id,
            json!({ "tools": tools }),
        )
    }

    async fn handle_call_tool(
        &self,
        id: Option<Value>,
        params: Option<Value>,
    ) -> JsonRpcResponse {
        let name = match params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
        {
            Some(name) => name.to_string(),
            None => {
                return JsonRpcResponse::error(id, -32602, "Missing required param: name");
            }
        };

        let arguments = params
            .as_ref()
            .and_then(|p| p.get("arguments"))
            .cloned()
            .unwrap_or(Value::Null);

        let result = match name.as_str() {
            "query_wiki" => self.tool_query_wiki(&arguments).await,
            "update_page" => self.tool_update_page(&arguments).await,
            "set_page_metadata" => self.tool_set_page_metadata(&arguments).await,
            "ingest_source" => self.tool_ingest_source(&arguments).await,
            "ingest_extracted_content" => self.tool_ingest_extracted(&arguments).await,
            "lint_wiki" => self.tool_lint_wiki(&arguments).await,
            "read_knowledge_uri" => self.tool_read_knowledge_uri(&arguments).await,
            "delete_page" => self.tool_delete_page(&arguments).await,
            "delete_raw_source" => self.tool_delete_raw_source(&arguments).await,
            "list_pages_by_type" => self.tool_list_pages_by_type(&arguments).await,
            "list_pages_by_project" => self.tool_list_pages_by_project(&arguments).await,
            "list_pages_by_tag" => self.tool_list_pages_by_tag(&arguments).await,
            "find_pages_without_sources" => self.tool_find_pages_without_sources(&arguments).await,
            "propose_page_update" => self.tool_propose_page_update(&arguments).await,
            "apply_page_update" => self.tool_apply_page_update(&arguments).await,
            "wiki_graph" => self.tool_wiki_graph(&arguments).await,
            "backlinks" => self.tool_backlinks(&arguments).await,
            "orphans" => self.tool_orphans(&arguments).await,
            "related_pages" => self.tool_related_pages(&arguments).await,
            "link_suggestions" => self.tool_link_suggestions(&arguments).await,
            "find_claims" => self.tool_find_claims(&arguments).await,
            "find_claims_without_source" => self.tool_find_claims_without_source(&arguments).await,
            "find_conflicting_claims" => self.tool_find_conflicting_claims(&arguments).await,
            "verify_claim" => self.tool_verify_claim(&arguments).await,
            _ => Err(format!("Tool not found: {name}")),
        };

        match result {
            Ok(content) => JsonRpcResponse::success(
                id,
                json!({ "content": content }),
            ),
            Err(e) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("Erro: {e}")}],
                    "isError": true
                }),
            ),
        }
    }

    // tool - query_wiki
    async fn tool_query_wiki(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: question")?;

        let max_pages = args
            .get("maxPages")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let include_raw = args
            .get("includeRawReferences")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");

        // Semântica desligada (qualquer um dos três `None`) ou modo explicitamente
        // `bm25` → caminho clássico, byte-a-byte idêntico ao de sempre.
        let semantic_ready = self.vector_store.is_some()
            && self.embed_provider.is_some()
            && self.semantic_cfg.is_some();
        if !semantic_ready || mode == "bm25" {
            return self.query_wiki_bm25(question, max_pages, include_raw).await;
        }

        self.query_wiki_hybrid(question, max_pages, include_raw, mode)
            .await
    }

    /// Busca BM25 pura — comportamento histórico do `query_wiki`. É o caminho
    /// quando a semântica está desligada, quando `mode=bm25`, e o fallback de
    /// degradação quando a query não embeda (auto/semantic caem para cá).
    async fn query_wiki_bm25(
        &self,
        question: &str,
        max_pages: usize,
        include_raw: bool,
    ) -> Result<Vec<McpToolContent>, String> {
        let results = if include_raw {
            self.search_engine.search(question, max_pages)
        } else {
            self.search_engine
                .search_with_kind_filter(question, max_pages, Some(DocumentKind::Page))
        }
        .map_err(|e| format!("Search error: {e}"))?;

        let mut lines = Vec::new();
        lines.push(format!("# Resultados para: \"{}\"\n", question));

        if results.is_empty() {
            lines.push("Nenhum resultado encontrado.".into());
        } else {
            for (i, r) in results.iter().enumerate() {
                lines.push(format!(
                    "## {}. {} (score: {:.2})\n- URI: `{}`\n- Trecho: {}\n",
                    i + 1,
                    r.title,
                    r.score,
                    r.uri,
                    r.snippet
                ));
            }
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: lines.join("\n"),
        }])
    }

    /// Busca híbrida: funde o ranking BM25 de páginas com o ranking semântico
    /// (cosseno força-bruta) por RRF. RAW continua SÓ BM25 (anexado ao fim).
    /// Degradação aditiva: query que não embeda cai para o BM25 puro.
    async fn query_wiki_hybrid(
        &self,
        question: &str,
        max_pages: usize,
        include_raw: bool,
        mode: &str,
    ) -> Result<Vec<McpToolContent>, String> {
        let store = self.vector_store.as_ref().unwrap();
        let provider = self.embed_provider.as_ref().unwrap();

        // Top-K alargado em cada lado para a fusão ter material; recortado a
        // `max_pages` só no fim.
        let top_k = max_pages.saturating_mul(3).max(max_pages);

        // Lado léxico: SÓ páginas (RAW é tratado à parte).
        let bm25_pages = self
            .search_engine
            .search_with_kind_filter(question, top_k, Some(DocumentKind::Page))
            .map_err(|e| format!("Search error: {e}"))?;

        // Embeda a query (sem prefixo — o default OpenAI não pede "query:"; a
        // família E5 pediria, mas não é o caso). Qualquer falha degrada pro BM25.
        let inputs = [question.to_string()];
        let query_vec = match provider.embed(&inputs).await {
            Ok(mut vecs) if vecs.first().map(|v| !v.is_empty()).unwrap_or(false) => vecs.remove(0),
            Ok(_) => {
                tracing::warn!("semântica: embed da query vazio — caindo pra BM25");
                return self.query_wiki_bm25(question, max_pages, include_raw).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "semântica: falha ao embedar a query — caindo pra BM25");
                return self.query_wiki_bm25(question, max_pages, include_raw).await;
            }
        };

        let sem_hits = store.semantic_search(&query_vec, top_k);

        // mode=semantic mas nada embedado ainda (store vazio) → cai pro BM25.
        if mode == "semantic" && sem_hits.is_empty() {
            return self.query_wiki_bm25(question, max_pages, include_raw).await;
        }

        let bm25_order: Vec<String> = bm25_pages
            .iter()
            .filter_map(|r| r.uri.strip_prefix("wiki://page/").map(|s| s.to_string()))
            .collect();
        let sem_order: Vec<String> = sem_hits.iter().map(|h| h.slug.clone()).collect();

        let fused: Vec<String> = match mode {
            "semantic" => sem_order.clone(),
            _ => rrf_fuse(&bm25_order, &sem_order, RRF_K, W_SEM, W_BM25),
        };

        let mut lines = Vec::new();
        lines.push(format!("# Resultados para: \"{}\"\n", question));
        let mut rank = 0usize;

        for slug in fused.iter().take(max_pages) {
            let uri = format!("wiki://page/{slug}");
            let sem_hit = sem_hits.iter().find(|h| &h.slug == slug);
            let bm25_hit = bm25_pages.iter().find(|r| r.uri == uri);
            if sem_hit.is_none() && bm25_hit.is_none() {
                continue; // slug sem fonte — não deveria ocorrer
            }
            let title = bm25_hit
                .map(|r| r.title.clone())
                .unwrap_or_else(|| slug.replace('-', " "));

            // Prefere o trecho semântico (chunk vencedor, fatiado dos offsets)
            // quando a página foi encontrada pela semântica; senão, o snippet BM25.
            let (score, origin, snippet) = if let Some(hit) = sem_hit {
                let snippet = self
                    .semantic_snippet(slug, hit)
                    .await
                    .or_else(|| bm25_hit.map(|r| r.snippet.clone()))
                    .unwrap_or_default();
                (hit.score, "via semântica", snippet)
            } else {
                let r = bm25_hit.unwrap();
                (r.score, "via BM25", r.snippet.clone())
            };

            rank += 1;
            lines.push(format!(
                "## {}. {} (score: {:.2}, {})\n- URI: `{}`\n- Trecho: {}\n",
                rank, title, score, origin, uri, snippet
            ));
        }

        // Raw sources (quando pedidas) continuam SÓ BM25, anexadas após as páginas.
        if include_raw {
            let raws = self
                .search_engine
                .search_with_kind_filter(question, max_pages, Some(DocumentKind::Raw))
                .map_err(|e| format!("Search error: {e}"))?;
            for r in &raws {
                rank += 1;
                lines.push(format!(
                    "## {}. {} (score: {:.2}, via BM25)\n- URI: `{}`\n- Trecho: {}\n",
                    rank, r.title, r.score, r.uri, r.snippet
                ));
            }
        }

        if rank == 0 {
            lines.push("Nenhum resultado encontrado.".into());
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: lines.join("\n"),
        }])
    }

    /// Fatia o chunk vencedor da página (corpo SEM frontmatter) pelos offsets
    /// guardados no embedding. `None` se a página sumiu ou os offsets ficaram
    /// fora de alcance (caímos no snippet BM25 nesse caso).
    async fn semantic_snippet(&self, slug: &str, hit: &SemanticHit) -> Option<String> {
        let content = self.file_manager.read_page(slug).await.ok()?;
        let body = crate::frontmatter::strip_frontmatter(&content);
        body.get(hit.best_start..hit.best_end)
            .map(|s| s.trim().to_string())
    }

    // tool - update_page
    async fn tool_update_page(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: mode")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: content")?;

        let rationale = args
            .get("rationale")
            .and_then(|v| v.as_str());

        let section = args.get("section").and_then(|v| v.as_str());

        let (raw_content, is_new_page) = if let Some(sec) = section {
            // Edição por seção: o servidor reconstrói o documento trocando só a
            // seção alvo. Exige página existente — não se cria uma seção numa
            // página que não existe (use 'section' ausente para criar a página).
            let existing = self.file_manager.read_page(slug).await.map_err(|_| {
                format!(
                    "Não é possível editar a seção '{sec}': a página '{slug}' não existe. \
                     Crie a página primeiro (sem o argumento 'section')."
                )
            })?;
            let section_mode = match mode {
                "overwrite" => crate::sections::SectionMode::Replace,
                "append" => crate::sections::SectionMode::Append,
                _ => return Err(format!("Invalid mode: {mode}. Use 'overwrite' or 'append'")),
            };
            let updated = crate::sections::edit_section(&existing, sec, content, section_mode)?;
            (updated, false)
        } else {
            match mode {
                "overwrite" => {
                    let is_new = self.file_manager.read_page(slug).await.is_err();
                    (content.to_string(), is_new)
                }
                "append" => {
                    match self.file_manager.read_page(slug).await {
                        Ok(existing) => (format!("{existing}\n\n{content}"), false),
                        Err(_) => (content.to_string(), true),
                    }
                }
                _ => return Err(format!("Invalid mode: {mode}. Use 'overwrite' or 'append'")),
            }
        };

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let final_content = crate::frontmatter::update_date_fields(&raw_content, &today, is_new_page);

        self.file_manager
            .write_page(slug, &final_content)
            .await
            .map_err(|e| format!("Write error: {e}"))?;

        // Registra no log se houver rationale
        if let Some(reason) = rationale {
            let scope = section.map(|s| format!(" seção '{s}'")).unwrap_or_default();
            let log_entry = format!("[{mode}{scope}] `{slug}`: {reason}");
            let _ = self.file_manager.append_to_log(&log_entry).await;
        }

        let scope = section
            .map(|s| format!(", seção: '{s}'"))
            .unwrap_or_default();
        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("Página `{slug}` atualizada com sucesso (modo: {mode}{scope})."),
        }])
    }

    // tool - set_page_metadata
    async fn tool_set_page_metadata(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let set = args.get("set").map(json_obj_to_string_pairs).unwrap_or_default();
        let add = args.get("add").map(json_obj_to_list_pairs).unwrap_or_default();
        let remove = args.get("remove").map(json_obj_to_list_pairs).unwrap_or_default();
        let rationale = args.get("rationale").and_then(|v| v.as_str());

        if set.is_empty() && add.is_empty() && remove.is_empty() {
            return Err("Informe ao menos um de 'set', 'add' ou 'remove'.".into());
        }

        // set_page_metadata edita; não cria página.
        let existing = self.file_manager.read_page(slug).await.map_err(|_| {
            format!(
                "A página '{slug}' não existe. set_page_metadata edita metadados de páginas \
                 existentes; crie a página com update_page primeiro."
            )
        })?;

        let updated = crate::frontmatter::apply_metadata(&existing, &set, &add, &remove)?;

        // Mesmo pipeline de gravação do update_page: datas gerenciadas + write.
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let final_content = crate::frontmatter::update_date_fields(&updated, &today, false);

        self.file_manager
            .write_page(slug, &final_content)
            .await
            .map_err(|e| format!("Write error: {e}"))?;

        if let Some(reason) = rationale {
            let log_entry = format!("[set_page_metadata] `{slug}`: {reason}");
            let _ = self.file_manager.append_to_log(&log_entry).await;
        }

        let mut changes: Vec<String> = Vec::new();
        for (k, v) in &set {
            changes.push(format!("set {k}={v}"));
        }
        for (k, items) in &add {
            changes.push(format!("add {k} += [{}]", items.join(", ")));
        }
        for (k, items) in &remove {
            changes.push(format!("remove {k} -= [{}]", items.join(", ")));
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("Metadados de `{slug}` atualizados: {}.", changes.join("; ")),
        }])
    }

    // tool - propose_page_update
    async fn tool_propose_page_update(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: content")?;

        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: reason")?;

        let section = args.get("section").and_then(|v| v.as_str());

        // Lê o estado atual da página (base da proposta).
        let base_content = self.file_manager.read_page(slug).await.ok();
        let base_page_exists = base_content.is_some();
        let base_for_diff = base_content.clone().unwrap_or_default();
        let base_hash = base_content
            .as_deref()
            .map(crate::change_plan::content_hash);

        // Com 'section', 'content' é só o corpo da seção: o servidor reconstrói
        // o documento completo trocando aquela seção (modo Replace). O diff
        // resultante fica pequeno e revisável. Sem 'section', 'content' é o
        // documento inteiro, como antes.
        let effective_content = match section {
            Some(sec) => {
                let existing = base_content.as_deref().ok_or_else(|| {
                    format!(
                        "Não é possível propor edição da seção '{sec}': a página '{slug}' não existe."
                    )
                })?;
                crate::sections::edit_section(
                    existing,
                    sec,
                    content,
                    crate::sections::SectionMode::Replace,
                )?
            }
            None => content.to_string(),
        };

        // Aplica os campos de data agora, para que o diff revisado seja
        // exatamente o conteúdo que será gravado na aplicação.
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let proposed_content =
            crate::frontmatter::update_date_fields(&effective_content, &today, !base_page_exists);

        let proposal_id = crate::change_plan::new_proposal_id(slug);
        let proposal = crate::change_plan::PageProposal::new(
            proposal_id.clone(),
            slug.to_string(),
            reason.to_string(),
            base_page_exists,
            base_hash,
            proposed_content.clone(),
        );

        self.file_manager
            .write_proposal(&proposal)
            .await
            .map_err(|e| format!("Erro ao salvar proposta: {e}"))?;

        let diff = crate::change_plan::render_unified_diff(&base_for_diff, &proposed_content);
        let (added, removed) =
            crate::change_plan::diff_stats(&base_for_diff, &proposed_content);

        let kind = if base_page_exists {
            "atualização"
        } else {
            "página nova"
        };
        let diff_block = if diff.trim().is_empty() {
            "_Sem alterações: o conteúdo proposto é idêntico ao atual._".to_string()
        } else {
            format!("```diff\n{diff}```")
        };

        let text = format!(
            "Proposta de alteração criada.\n\n\
             - proposal_id: `{proposal_id}`\n\
             - página: `{slug}` ({kind})\n\
             - linhas: +{added} / -{removed}\n\
             - motivo: {reason}\n\n\
             ## Diff\n\n{diff_block}\n\n\
             Para aplicar, chame `apply_page_update` com `proposalId: \"{proposal_id}\"`."
        );

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - apply_page_update
    async fn tool_apply_page_update(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let proposal_id = args
            .get("proposalId")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: proposalId")?;

        let force = args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut proposal = self
            .file_manager
            .read_proposal(proposal_id)
            .await
            .map_err(|e| format!("Proposta `{proposal_id}` não encontrada: {e}"))?;

        if !proposal.is_pending() {
            return Err(format!(
                "Proposta `{proposal_id}` já foi aplicada em {}.",
                proposal.applied_at.as_deref().unwrap_or("data desconhecida")
            ));
        }

        // Revalida que a página base não mudou desde a criação da proposta.
        let current = self.file_manager.read_page(&proposal.slug).await.ok();
        let current_exists = current.is_some();
        let current_hash = current.as_deref().map(crate::change_plan::content_hash);
        let base_changed =
            current_exists != proposal.base_page_exists || current_hash != proposal.base_hash;

        if base_changed && !force {
            let detail = if current_exists != proposal.base_page_exists {
                if current_exists {
                    "a página passou a existir desde a proposta"
                } else {
                    "a página foi removida desde a proposta"
                }
            } else {
                "a página foi modificada desde a proposta"
            };
            return Err(format!(
                "Conflito: {detail}. Crie um novo `propose_page_update` para revisar, \
                 ou repita `apply_page_update` com `force: true` para sobrescrever."
            ));
        }

        self.file_manager
            .write_page(&proposal.slug, &proposal.proposed_content)
            .await
            .map_err(|e| format!("Erro ao gravar página: {e}"))?;

        let log_entry = format!(
            "[apply_page_update] `{}` (proposta {}): {}",
            proposal.slug, proposal.proposal_id, proposal.reason
        );
        let _ = self.file_manager.append_to_log(&log_entry).await;

        // Marca a proposta como aplicada (registro de auditoria).
        proposal.status = crate::change_plan::STATUS_APPLIED.to_string();
        proposal.applied_at = Some(chrono::Utc::now().to_rfc3339());
        if let Err(e) = self.file_manager.write_proposal(&proposal).await {
            tracing::warn!(error = %e, "Falha ao marcar proposta como aplicada");
        }

        let forced_note = if base_changed && force {
            "\n- aviso: a página havia mudado desde a proposta; aplicada com `force`."
        } else {
            ""
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!(
                "Proposta `{}` aplicada à página `{}`.{forced_note}",
                proposal.proposal_id, proposal.slug
            ),
        }])
    }

    // tool - ingest_source
    async fn tool_ingest_source(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let source_uri = args
            .get("sourceUri")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: sourceUri")?;

        let source_type = args
            .get("sourceType")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: sourceType")?;

        let source_id = Self::build_source_id(source_uri);

        let force = args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !force {
            let existing = self
                .file_manager
                .list_raw_sources()
                .await
                .map_err(|e| format!("Erro ao listar sources: {e}"))?;
            if existing.contains(&source_id) {
                return Err(format!(
                    "Source '{}' já existe. Use force: true para sobrescrever.",
                    source_id
                ));
            }
        }

        let (bytes, detected_mime) = Self::load_source_content(source_uri).await?;
        let raw_content = String::from_utf8(bytes)
            .map_err(|_| "A source ingerida não é texto UTF-8 válido.".to_string())?;

        let mut metadata = crate::storage::RawSourceMetadata::new(source_id.clone(), raw_content.len() as u64);
        metadata.original_path = Some(source_uri.to_string());
        metadata.mime_type = detected_mime.or_else(|| Some(match source_type {
            "markdown" | "md" => "text/markdown".into(),
            "html" => "text/html".into(),
            "json" => "application/json".into(),
            _ => "text/plain".into(),
        }));
        metadata.tags = vec![source_type.to_string()];

        self.file_manager
            .write_raw_source(&source_id, &metadata, &raw_content)
            .await
            .map_err(|e| format!("Ingest error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!(
                "Source ingerida com sucesso.\n- source_id: `{source_id}`\n- logical_uri: `raw://source/{source_id}`\n- bytes: {}\n- mime_type: {}",
                raw_content.len(),
                metadata.mime_type.as_deref().unwrap_or("desconhecido")
            ),
        }])
    }

    // tool - ingest_extracted_content
    async fn tool_ingest_extracted(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let logical_uri = args
            .get("logicalUri")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: logicalUri")?;

        let source_type = args
            .get("sourceType")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: sourceType")?;

        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: title")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: content")?;

        // Extrai source_id da logicalUri (precisa ter o prefixo raw://source/)
        let source_id = logical_uri
            .strip_prefix("raw://source/")
            .ok_or_else(|| format!(
                "logicalUri inválida: '{logical_uri}'. Esperado formato 'raw://source/<id>'."
            ))?
            .to_string();

        // Verifica force — protege contra sobrescrita acidental
        let force = args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !force {
            let existing = self
                .file_manager
                .list_raw_sources()
                .await
                .map_err(|e| format!("Erro ao listar sources: {e}"))?;
            if existing.contains(&source_id) {
                return Err(format!(
                    "Source '{}' já existe. Use force: true para sobrescrever.",
                    source_id
                ));
            }
        }

        let mut metadata = crate::storage::RawSourceMetadata::new(
            source_id.clone(),
            content.len() as u64,
        );
        metadata.mime_type = Some(match source_type {
            "pdf" => "application/pdf".into(),
            "markdown" | "md" => "text/markdown".into(),
            _ => "text/plain".into(),
        });
        metadata.tags = vec![source_type.to_string(), title.to_string()];

        self.file_manager
            .write_raw_source(&source_id, &metadata, content)
            .await
            .map_err(|e| format!("Ingest error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!(
                "Conteúdo extraído salvo com sucesso.\n- source_id: `{source_id}`\n- title: {title}\n- size: {} bytes",
                content.len()
            ),
        }])
    }

    // tool - lint_wiki
    async fn tool_lint_wiki(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: scope")?;

        let report = crate::lint::run_lint(
            scope,
            &self.file_manager,
            &self.search_engine,
            self.semantic_cfg.as_ref(),
        )
        .await
        .map_err(|e| format!("Lint error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: report.format_markdown(),
        }])
    }

    // tool - delete_page
    async fn tool_delete_page(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let rationale = args.get("rationale").and_then(|v| v.as_str());

        self.file_manager
            .delete_page(slug)
            .await
            .map_err(|e| format!("Delete error: {e}"))?;

        if let Some(reason) = rationale {
            let log_entry = format!("[delete_page] `{slug}`: {reason}");
            let _ = self.file_manager.append_to_log(&log_entry).await;
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("Página `{slug}` removida."),
        }])
    }

    // tool - delete_raw_source
    async fn tool_delete_raw_source(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let source_id = args
            .get("sourceId")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: sourceId")?;

        let rationale = args.get("rationale").and_then(|v| v.as_str());

        self.file_manager
            .delete_raw_source(source_id)
            .await
            .map_err(|e| format!("Delete error: {e}"))?;

        if let Some(reason) = rationale {
            let log_entry = format!("[delete_raw_source] `{source_id}`: {reason}");
            let _ = self.file_manager.append_to_log(&log_entry).await;
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("Raw source `{source_id}` removida."),
        }])
    }

    // tool - read_knowledge_uri
    async fn tool_read_knowledge_uri(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let uri = args
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: uri")?;

        let (content, mime_type) = self
            .read_resource_by_uri(uri)
            .await
            .map_err(|e| format!("Read error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("<!-- mime: {mime_type} -->\n{content}"),
        }])
    }

    // tool - list_pages_by_type
    async fn tool_list_pages_by_type(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let page_type = args
            .get("pageType")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: pageType")?
            .to_lowercase();

        let matches = self.pages_matching(|fm| {
            fm.page_type.as_deref().map(|t| t.to_lowercase()) == Some(page_type.clone())
        }).await.map_err(|e| format!("List error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format_page_match_list(&format!("type: \"{page_type}\""), &matches),
        }])
    }

    // tool - list_pages_by_project
    async fn tool_list_pages_by_project(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: project")?
            .to_lowercase();

        let matches = self.pages_matching(|fm| {
            fm.project.as_deref().map(|p| p.to_lowercase()) == Some(project.clone())
        }).await.map_err(|e| format!("List error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format_page_match_list(&format!("project: \"{project}\""), &matches),
        }])
    }

    // tool - list_pages_by_tag
    async fn tool_list_pages_by_tag(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let tag = args
            .get("tag")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: tag")?
            .to_lowercase();

        let matches = self.pages_matching(|fm| {
            fm.tags.iter().any(|t| t.to_lowercase() == tag)
        }).await.map_err(|e| format!("List error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format_page_match_list(&format!("tag: \"{tag}\""), &matches),
        }])
    }

    // tool - find_pages_without_sources
    async fn tool_find_pages_without_sources(&self, _args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slugs = self.file_manager
            .list_pages()
            .await
            .map_err(|e| format!("List error: {e}"))?;

        let mut results: Vec<String> = Vec::new();
        for slug in &slugs {
            match self.file_manager.read_page(slug).await {
                Ok(content) => {
                    let no_sources = match crate::frontmatter::parse_frontmatter(&content) {
                        Some(fm) => fm.sources.is_empty(),
                        None => true,
                    };
                    if no_sources {
                        results.push(slug.clone());
                    }
                }
                Err(_) => results.push(slug.clone()),
            }
        }

        let text = if results.is_empty() {
            "_Todas as páginas têm pelo menos uma source documentada no frontmatter._".to_string()
        } else {
            let mut lines = vec!["# Páginas sem Sources Documentadas\n".to_string()];
            for slug in &results {
                lines.push(format!("- `{slug}`"));
            }
            lines.join("\n")
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - wiki_graph
    async fn tool_wiki_graph(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("summary");

        if !matches!(format, "summary" | "full" | "mermaid") {
            return Err(format!(
                "Formato inválido: '{format}'. Use 'summary', 'full' ou 'mermaid'."
            ));
        }

        let graph = crate::graph::WikiGraph::build(&self.file_manager)
            .await
            .map_err(|e| format!("Graph error: {e}"))?;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: graph.render(format),
        }])
    }

    // tool - backlinks
    async fn tool_backlinks(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let graph = crate::graph::WikiGraph::build(&self.file_manager)
            .await
            .map_err(|e| format!("Graph error: {e}"))?;

        if !graph.contains(slug) {
            return Err(format!("Página '{slug}' não existe na Wiki."));
        }

        let backlinks = graph.backlinks(slug);
        let text = if backlinks.is_empty() {
            format!(
                "# Backlinks para `{slug}`\n\n_Nenhuma página aponta para esta — é uma página órfã._"
            )
        } else {
            let mut lines = vec![format!("# Backlinks para `{slug}`\n")];
            for (src, kind) in backlinks {
                lines.push(format!("- `{src}` ({})", crate::graph::edge_kind_label(kind)));
            }
            lines.join("\n")
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - orphans
    async fn tool_orphans(&self, _args: &Value) -> Result<Vec<McpToolContent>, String> {
        let graph = crate::graph::WikiGraph::build(&self.file_manager)
            .await
            .map_err(|e| format!("Graph error: {e}"))?;

        let orphans = graph.orphans();
        let text = if orphans.is_empty() {
            "_Nenhuma página órfã — toda página recebe ao menos um link._".to_string()
        } else {
            let mut lines = vec![
                "# Páginas Órfãs\n".to_string(),
                "_Nenhuma outra página aponta para estas (links da página `index` gerada são ignorados)._\n".to_string(),
            ];
            for slug in &orphans {
                lines.push(format!("- `{slug}`"));
            }
            lines.join("\n")
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - related_pages
    async fn tool_related_pages(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let graph = crate::graph::WikiGraph::build(&self.file_manager)
            .await
            .map_err(|e| format!("Graph error: {e}"))?;

        if !graph.contains(slug) {
            return Err(format!("Página '{slug}' não existe na Wiki."));
        }

        let related = graph.related(slug);
        let text = if related.is_empty() {
            format!("# Páginas Relacionadas a `{slug}`\n\n_Nenhuma página relacionada encontrada._")
        } else {
            let mut lines = vec![format!("# Páginas Relacionadas a `{slug}`\n")];
            for entry in related {
                lines.push(format!("- `{}` — {}", entry.slug, entry.relation.label()));
            }
            lines.join("\n")
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - link_suggestions
    async fn tool_link_suggestions(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let focus = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug);

        let max_suggestions = args
            .get("maxSuggestions")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let min_similarity = args
            .get("minSimilarity")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.15) as f32;

        let graph = crate::graph::WikiGraph::build(&self.file_manager)
            .await
            .map_err(|e| format!("Graph error: {e}"))?;

        if let Some(f) = focus {
            if !graph.contains(f) {
                return Err(format!("Página '{f}' não existe na Wiki."));
            }
        }

        let suggestions = crate::graph::suggest_links(
            &self.file_manager,
            &graph,
            focus,
            min_similarity,
            max_suggestions,
        )
        .await
        .map_err(|e| format!("Suggestion error: {e}"))?;

        let text = if suggestions.is_empty() {
            "_Nenhuma sugestão de link encontrada com os critérios atuais._".to_string()
        } else {
            let mut lines = vec!["# Sugestões de Links\n".to_string()];
            for s in &suggestions {
                lines.push(format!(
                    "- `{}` ⇄ `{}` (score {:.2}) — {}",
                    s.from,
                    s.to,
                    s.score,
                    s.reasons.join("; ")
                ));
            }
            lines.join("\n")
        };

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text,
        }])
    }

    // tool - find_claims
    async fn tool_find_claims(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let focus = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug);

        let slugs: Vec<String> = match focus {
            Some(s) => {
                if self.file_manager.read_page(s).await.is_err() {
                    return Err(format!("Página '{s}' não existe na Wiki."));
                }
                vec![s.to_string()]
            }
            None => self
                .file_manager
                .list_pages()
                .await
                .map_err(|e| format!("List error: {e}"))?,
        };

        let mut lines = vec!["# Claims".to_string()];
        let mut total = 0;
        for slug in &slugs {
            let content = match self.file_manager.read_page(slug).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let claims = crate::claims::parse_claims(&content);
            if claims.is_empty() {
                continue;
            }
            lines.push(String::new());
            lines.push(format!("## `{slug}`"));
            for (i, claim) in claims.iter().enumerate() {
                total += 1;
                lines.push(format!("\n{}. {}", i + 1, claim.text));
                lines.push(format!(
                    "   - Source: {}",
                    claim.source.as_deref().unwrap_or("_(ausente)_")
                ));
                lines.push(format!(
                    "   - Confidence: {}",
                    claim.confidence.as_deref().unwrap_or("_(ausente)_")
                ));
                lines.push(format!(
                    "   - Last verified: {}",
                    claim.last_verified.as_deref().unwrap_or("_(ausente)_")
                ));
            }
        }

        if total == 0 {
            let text = match focus {
                Some(s) => format!("_A página `{s}` não tem claims registrados._"),
                None => "_Nenhuma página tem claims registrados._".to_string(),
            };
            return Ok(vec![McpToolContent {
                content_type: "text".into(),
                text,
            }]);
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: lines.join("\n"),
        }])
    }

    // tool - find_claims_without_source
    async fn tool_find_claims_without_source(
        &self,
        _args: &Value,
    ) -> Result<Vec<McpToolContent>, String> {
        let slugs = self
            .file_manager
            .list_pages()
            .await
            .map_err(|e| format!("List error: {e}"))?;

        let mut lines = vec!["# Claims sem Source\n".to_string()];
        let mut total = 0;
        for slug in &slugs {
            let content = match self.file_manager.read_page(slug).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let claims = crate::claims::parse_claims(&content);
            let missing: Vec<(usize, &crate::claims::Claim)> = claims
                .iter()
                .enumerate()
                .filter(|(_, c)| c.source.is_none())
                .collect();
            if missing.is_empty() {
                continue;
            }
            lines.push(format!("## `{slug}`"));
            for (i, claim) in missing {
                total += 1;
                lines.push(format!("- [{}] {}", i + 1, claim.text));
            }
            lines.push(String::new());
        }

        if total == 0 {
            return Ok(vec![McpToolContent {
                content_type: "text".into(),
                text: "_Todos os claims têm Source documentada._".to_string(),
            }]);
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: lines.join("\n"),
        }])
    }

    // tool - find_conflicting_claims
    async fn tool_find_conflicting_claims(
        &self,
        args: &Value,
    ) -> Result<Vec<McpToolContent>, String> {
        let min_similarity = args
            .get("minSimilarity")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.25) as f32;

        let slugs = self
            .file_manager
            .list_pages()
            .await
            .map_err(|e| format!("List error: {e}"))?;

        struct ClaimRef {
            slug: String,
            index: usize,
            text: String,
            tokens: std::collections::HashSet<String>,
        }

        let mut all: Vec<ClaimRef> = Vec::new();
        for slug in &slugs {
            let content = match self.file_manager.read_page(slug).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (i, claim) in crate::claims::parse_claims(&content).into_iter().enumerate() {
                let tokens = crate::lint::tokenize(&claim.text);
                all.push(ClaimRef {
                    slug: slug.clone(),
                    index: i + 1,
                    text: claim.text,
                    tokens,
                });
            }
        }

        let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let sim = crate::lint::jaccard(&all[i].tokens, &all[j].tokens);
                if sim >= min_similarity {
                    pairs.push((i, j, sim));
                }
            }
        }
        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let truncated = pairs.len() > 50;
        pairs.truncate(50);

        if pairs.is_empty() {
            return Ok(vec![McpToolContent {
                content_type: "text".into(),
                text: "_Nenhum par de claims com vocabulário sobreposto acima do limite._"
                    .to_string(),
            }]);
        }

        let mut lines = vec![
            "# Claims Candidatos a Conflito\n".to_string(),
            "_Pares de claims com vocabulário sobreposto. Triagem heurística — revise se realmente se contradizem._\n".to_string(),
        ];
        for (i, j, sim) in pairs {
            let a = &all[i];
            let b = &all[j];
            lines.push(format!(
                "- `{}` [{}] \"{}\"  ⇄  `{}` [{}] \"{}\" — {:.0}% de termos em comum",
                a.slug, a.index, a.text, b.slug, b.index, b.text, sim * 100.0
            ));
        }
        if truncated {
            lines.push("\n_(exibindo os 50 pares de maior sobreposição)_".to_string());
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: lines.join("\n"),
        }])
    }

    // tool - verify_claim
    async fn tool_verify_claim(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(normalize_page_slug)
            .ok_or("Missing required arg: slug")?;

        let claim_index = args
            .get("claimIndex")
            .and_then(|v| v.as_u64())
            .ok_or("Missing required arg: claimIndex")?;
        if claim_index < 1 {
            return Err("claimIndex deve ser >= 1".to_string());
        }

        let date = args
            .get("date")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());

        let content = self
            .file_manager
            .read_page(slug)
            .await
            .map_err(|e| format!("Página '{slug}' não encontrada: {e}"))?;

        let updated = crate::claims::set_last_verified(&content, (claim_index - 1) as usize, &date)?;

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let final_content = crate::frontmatter::update_date_fields(&updated, &today, false);

        self.file_manager
            .write_page(slug, &final_content)
            .await
            .map_err(|e| format!("Write error: {e}"))?;

        let log_entry = format!("[verify_claim] `{slug}` claim {claim_index}: verificado em {date}");
        let _ = self.file_manager.append_to_log(&log_entry).await;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!(
                "Claim {claim_index} de `{slug}` marcado como verificado.\n- Last verified: {date}"
            ),
        }])
    }

    /// lê todas as páginas e retorna os slugs cujo frontmatter satisfaz `predicate`.
    async fn pages_matching(
        &self,
        predicate: impl Fn(&crate::frontmatter::PageFrontmatter) -> bool,
    ) -> anyhow::Result<Vec<(String, crate::frontmatter::PageFrontmatter)>> {
        let slugs = self.file_manager.list_pages().await?;
        let mut matches = Vec::new();

        for slug in &slugs {
            if let Ok(content) = self.file_manager.read_page(slug).await {
                if let Some(fm) = crate::frontmatter::parse_frontmatter(&content) {
                    if predicate(&fm) {
                        matches.push((slug.clone(), fm));
                    }
                }
            }
        }

        matches.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(matches)
    }
}

/// aceita um slug puro ou uma URI `wiki://page/{slug}` e retorna sempre o slug.
fn normalize_page_slug(input: &str) -> &str {
    input.strip_prefix("wiki://page/").unwrap_or(input)
}

/// Converte um objeto JSON `{chave: valor_escalar}` em pares `(String, String)`.
/// Valores string/número/bool viram texto; outros tipos são ignorados.
fn json_obj_to_string_pairs(v: &Value) -> Vec<(String, String)> {
    v.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| {
                    let s = match val {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((k.clone(), s))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Converte um objeto JSON `{chave: [item, ...]}` em pares `(String, Vec<String>)`.
fn json_obj_to_list_pairs(v: &Value) -> Vec<(String, Vec<String>)> {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, val)| {
                    let items = val
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| match x {
                                    Value::String(s) => Some(s.clone()),
                                    Value::Number(n) => Some(n.to_string()),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (k.clone(), items)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn format_page_match_list(
    filter_desc: &str,
    matches: &[(String, crate::frontmatter::PageFrontmatter)],
) -> String {
    if matches.is_empty() {
        return format!("_Nenhuma página encontrada com {filter_desc}._");
    }
    let mut lines = vec![format!("# Páginas com {filter_desc}\n")];
    for (slug, fm) in matches {
        let mut meta = Vec::new();
        if let Some(t) = &fm.page_type { meta.push(format!("type: {t}")); }
        if let Some(p) = &fm.project  { meta.push(format!("project: {p}")); }
        if let Some(s) = &fm.status   { meta.push(format!("status: {s}")); }
        let suffix = if meta.is_empty() {
            String::new()
        } else {
            format!(" ({})", meta.join(", "))
        };
        lines.push(format!("- `{slug}`{suffix}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::DocumentKind;
    use tempfile::TempDir;

    #[test]
    fn test_json_rpc_response_success() {
        let response = JsonRpcResponse::success(
            Some(Value::Number(1.into())),
            json!({"status": "ok"}),
        );
        let json_str = serde_json::to_string(&response).unwrap();
        assert!(json_str.contains("\"result\""));
        assert!(json_str.contains("\"status\":\"ok\""));
        assert!(!json_str.contains("\"error\""));
    }

    #[test]
    fn test_json_rpc_response_error() {
        let response = JsonRpcResponse::error(
            Some(Value::Number(2.into())),
            -32601,
            "Method not found",
        );
        let json_str = serde_json::to_string(&response).unwrap();
        assert!(json_str.contains("\"error\""));
        assert!(json_str.contains("-32601"));
        assert!(!json_str.contains("\"result\""));
    }

    #[test]
    fn test_json_rpc_request_deserialize() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(Value::Number(1.into())));
    }

    #[test]
    fn test_json_rpc_notification_no_id() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "notifications/initialized");
        assert!(req.id.is_none());
    }

    #[test]
    fn test_json_rpc_request_version_check() {
        let valid: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"initialize"}"#,
        )
        .unwrap();
        let invalid: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"1.0","method":"initialize"}"#,
        )
        .unwrap();

        assert!(valid.is_jsonrpc_2_0());
        assert!(!invalid.is_jsonrpc_2_0());
    }

    #[test]
    fn test_guess_mime_from_path() {
        assert_eq!(
            guess_mime_from_path(Path::new("arquivo.md")),
            Some("text/markdown".to_string())
        );
        assert_eq!(
            guess_mime_from_path(Path::new("dados.json")),
            Some("application/json".to_string())
        );
        assert_eq!(guess_mime_from_path(Path::new("sem_ext")), None);
    }

    #[test]
    fn test_build_source_id_is_stable() {
        let a = AdvWikiMcpServer::build_source_id("https://example.com/data.txt");
        let b = AdvWikiMcpServer::build_source_id("https://example.com/data.txt");
        let c = AdvWikiMcpServer::build_source_id("https://example.com/other.txt");

        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[tokio::test]
    async fn test_tool_query_wiki_prefers_page_filter_before_top_k() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let file_manager = Arc::new(WikiFileManager::new(Some(root.clone())));
        file_manager.init().await.unwrap();

        let search_engine = Arc::new(WikiSearchEngine::new(root.join(".advwiki/index")).unwrap());

        for i in 0..3 {
            search_engine
                .index_document(
                    DocumentKind::Raw,
                    &format!("raw://source/{i}"),
                    &format!("raw-{i}"),
                    "alpha alpha alpha alpha alpha",
                    1000,
                )
                .unwrap();
        }

        search_engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/alpha",
                "Alpha",
                "alpha única página relevante",
                1000,
            )
            .unwrap();

        let server = AdvWikiMcpServer::new(file_manager, search_engine, None, None, None);
        let response = server
            .tool_query_wiki(&json!({
                "question": "alpha",
                "maxPages": 1,
                "includeRawReferences": false
            }))
            .await
            .unwrap();

        assert_eq!(response.len(), 1);
        assert!(response[0].text.contains("wiki://page/alpha"));
        assert!(!response[0].text.contains("raw://source/"));
    }

    /// Extrai o `proposal_id` do texto retornado por `propose_page_update`.
    fn extract_proposal_id(text: &str) -> String {
        let marker = "proposal_id: `";
        let start = text.find(marker).expect("proposal_id ausente no output") + marker.len();
        let end = text[start..].find('`').expect("fim do proposal_id") + start;
        text[start..end].to_string()
    }

    async fn make_server(root: &std::path::Path) -> AdvWikiMcpServer {
        let file_manager = Arc::new(WikiFileManager::new(Some(root.to_path_buf())));
        file_manager.init().await.unwrap();
        let search_engine =
            Arc::new(WikiSearchEngine::new(root.join(".advwiki/index")).unwrap());
        AdvWikiMcpServer::new(file_manager, search_engine, None, None, None)
    }

    #[test]
    fn test_normalize_page_slug() {
        assert_eq!(normalize_page_slug("home"), "home");
        assert_eq!(normalize_page_slug("wiki://page/home"), "home");
        assert_eq!(normalize_page_slug("wiki://page/getting-started"), "getting-started");
    }

    #[tokio::test]
    async fn test_update_page_accepts_wiki_uri_slug() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        // Forma URI (wiki://page/...) deve ser normalizada — sem isso, validate_slug
        // rejeitaria o `/` e `:` com erro de slug inválido.
        server
            .tool_update_page(&json!({
                "slug": "wiki://page/home",
                "mode": "overwrite",
                "content": "# Home"
            }))
            .await
            .expect("update_page deve aceitar a forma wiki://page/");

        // Gravou na página `home` (e injetou datas — Bug 1).
        let content = server.file_manager.read_page("home").await.unwrap();
        assert!(content.contains("# Home"));
        assert!(content.contains("updated_at"));
        assert!(content.contains("created_at"));
    }

    #[tokio::test]
    async fn test_update_page_section_overwrite_preserves_rest() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "content": "---\ntype: service\n---\n\n# Serviço\n\n## Detalhes\n\nCorpo antigo.\n\n## Veja também\n\n- [[outra]]\n"
            }))
            .await
            .unwrap();

        // edita só a seção Detalhes
        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "section": "Detalhes",
                "content": "Corpo NOVO da seção."
            }))
            .await
            .unwrap();

        let content = server.file_manager.read_page("svc").await.unwrap();
        assert!(content.contains("Corpo NOVO da seção."));
        assert!(!content.contains("Corpo antigo."));
        // o resto da página é preservado
        assert!(content.contains("# Serviço"));
        assert!(content.contains("## Veja também"));
        assert!(content.contains("- [[outra]]"));
        // datas continuam gerenciadas
        assert!(content.contains("updated_at"));
    }

    #[tokio::test]
    async fn test_update_page_section_append_keeps_existing() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "content": "# T\n\n## Notas\n\nprimeira nota\n"
            }))
            .await
            .unwrap();

        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "append",
                "section": "Notas",
                "content": "segunda nota"
            }))
            .await
            .unwrap();

        let content = server.file_manager.read_page("svc").await.unwrap();
        assert!(content.contains("primeira nota"));
        assert!(content.contains("segunda nota"));
    }

    #[tokio::test]
    async fn test_update_page_section_missing_page_errors() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        let err = server
            .tool_update_page(&json!({
                "slug": "fantasma",
                "mode": "overwrite",
                "section": "Detalhes",
                "content": "x"
            }))
            .await
            .unwrap_err();
        assert!(err.contains("não existe"));
    }

    #[tokio::test]
    async fn test_update_page_section_unknown_section_errors() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "content": "# T\n\n## Detalhes\n\nx\n"
            }))
            .await
            .unwrap();

        let err = server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "section": "Inexistente",
                "content": "y"
            }))
            .await
            .unwrap_err();
        assert!(err.contains("não encontrada"));
    }

    #[tokio::test]
    async fn test_propose_page_update_section_produces_small_diff() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "content": "# T\n\n## Detalhes\n\nantigo\n\n## Veja também\n\n- [[x]]\n"
            }))
            .await
            .unwrap();

        let resp = server
            .tool_propose_page_update(&json!({
                "slug": "svc",
                "section": "Detalhes",
                "content": "conteúdo proposto",
                "reason": "revisar detalhes"
            }))
            .await
            .unwrap();
        let text = &resp[0].text;

        // o diff cobre só a seção: menciona o novo e o antigo conteúdo da seção,
        // mas não toca na seção "Veja também".
        assert!(text.contains("conteúdo proposto"));
        assert!(text.contains("-antigo") || text.contains("antigo"));
        assert!(!text.contains("- [[x]]"), "o diff não deve mexer em outras seções");

        // a proposta é aplicável e preserva o resto
        let proposal_id = extract_proposal_id(text);
        server
            .tool_apply_page_update(&json!({ "proposalId": proposal_id }))
            .await
            .unwrap();
        let content = server.file_manager.read_page("svc").await.unwrap();
        assert!(content.contains("conteúdo proposto"));
        assert!(!content.contains("antigo"));
        assert!(content.contains("## Veja também"));
        assert!(content.contains("- [[x]]"));
    }

    // ── set_page_metadata ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_set_page_metadata_sets_scalar_and_keeps_body() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;
        server
            .tool_update_page(&json!({
                "slug": "svc",
                "mode": "overwrite",
                "content": "---\ntype: service\nstatus: draft\n---\n\n# Serviço\n\ncorpo importante\n"
            }))
            .await
            .unwrap();

        server
            .tool_set_page_metadata(&json!({
                "slug": "svc",
                "set": { "status": "active", "project": "auth" }
            }))
            .await
            .unwrap();

        let content = server.file_manager.read_page("svc").await.unwrap();
        let fm = crate::frontmatter::parse_frontmatter(&content).unwrap();
        assert_eq!(fm.status.as_deref(), Some("active"));
        assert_eq!(fm.project.as_deref(), Some("auth"));
        assert_eq!(fm.page_type.as_deref(), Some("service"));
        assert!(content.contains("corpo importante"), "o corpo não pode ser tocado");
        assert!(content.contains("updated_at"));
    }

    #[tokio::test]
    async fn test_set_page_metadata_add_and_remove_tags() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;
        server
            .tool_update_page(&json!({
                "slug": "p",
                "mode": "overwrite",
                "content": "---\ntype: note\ntags:\n  - a\n---\nx"
            }))
            .await
            .unwrap();

        server
            .tool_set_page_metadata(&json!({
                "slug": "p",
                "add": { "tags": ["a", "b", "c"] },
                "remove": { "tags": ["a"] }
            }))
            .await
            .unwrap();

        let content = server.file_manager.read_page("p").await.unwrap();
        let fm = crate::frontmatter::parse_frontmatter(&content).unwrap();
        // 'a' removida, 'b' e 'c' adicionadas sem duplicar
        assert_eq!(fm.tags, vec!["b", "c"]);
    }

    #[tokio::test]
    async fn test_set_page_metadata_missing_page_errors() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;
        let err = server
            .tool_set_page_metadata(&json!({ "slug": "fantasma", "set": { "status": "active" } }))
            .await
            .unwrap_err();
        assert!(err.contains("não existe"));
    }

    #[tokio::test]
    async fn test_set_page_metadata_requires_an_operation() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;
        server
            .tool_update_page(&json!({ "slug": "p", "mode": "overwrite", "content": "# P" }))
            .await
            .unwrap();
        let err = server
            .tool_set_page_metadata(&json!({ "slug": "p" }))
            .await
            .unwrap_err();
        assert!(err.contains("ao menos um"));
    }

    #[tokio::test]
    async fn test_set_page_metadata_rejects_managed_date() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;
        server
            .tool_update_page(&json!({ "slug": "p", "mode": "overwrite", "content": "# P" }))
            .await
            .unwrap();
        let err = server
            .tool_set_page_metadata(&json!({ "slug": "p", "set": { "updated_at": "2020-01-01" } }))
            .await
            .unwrap_err();
        assert!(err.contains("gerenciado automaticamente"));
    }

    #[tokio::test]
    async fn test_delete_page_accepts_wiki_uri_slug() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server.file_manager.write_page("home", "# Home").await.unwrap();
        server
            .tool_delete_page(&json!({ "slug": "wiki://page/home" }))
            .await
            .expect("delete_page deve aceitar a forma wiki://page/");

        assert!(server.file_manager.read_page("home").await.is_err());
    }

    /// monta um servidor de teste com uma Wiki vazia inicializada.
    async fn make_test_server(root: std::path::PathBuf) -> AdvWikiMcpServer {
        let file_manager = Arc::new(WikiFileManager::new(Some(root.clone())));
        file_manager.init().await.unwrap();
        let search_engine = Arc::new(WikiSearchEngine::new(root.join(".advwiki/index")).unwrap());
        AdvWikiMcpServer::new(file_manager, search_engine, None, None, None)
    }

    #[tokio::test]
    async fn test_propose_page_update_creates_reviewable_proposal() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        let result = server
            .tool_propose_page_update(&json!({
                "slug": "novidade",
                "content": "# Novidade\n\nconteúdo novo",
                "reason": "criar página"
            }))
            .await
            .unwrap();

        let text = &result[0].text;
        assert!(text.contains("página nova"), "{text}");
        assert!(text.contains("+# Novidade"), "{text}");

        // a proposta foi persistida, mas a página ainda NÃO foi gravada
        let id = extract_proposal_id(text);
        let proposal = server.file_manager.read_proposal(&id).await.unwrap();
        assert_eq!(proposal.slug, "novidade");
        assert!(proposal.is_pending());
        assert!(server.file_manager.read_page("novidade").await.is_err());
    }

    #[tokio::test]
    async fn test_apply_page_update_writes_page_and_logs() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        let propose = server
            .tool_propose_page_update(&json!({
                "slug": "guia",
                "content": "# Guia\n\npasso a passo",
                "reason": "documentar o guia"
            }))
            .await
            .unwrap();
        let id = extract_proposal_id(&propose[0].text);

        let apply = server
            .tool_apply_page_update(&json!({ "proposalId": id.as_str() }))
            .await
            .unwrap();
        assert!(apply[0].text.contains("aplicada"));

        assert!(
            server
                .file_manager
                .read_page("guia")
                .await
                .unwrap()
                .contains("passo a passo")
        );

        let log = server.file_manager.read_log().await.unwrap();
        assert!(log.contains("apply_page_update"));
        assert!(log.contains("documentar o guia"));

        let proposal = server.file_manager.read_proposal(&id).await.unwrap();
        assert!(!proposal.is_pending());
        assert!(proposal.applied_at.is_some());
    }

    #[tokio::test]
    async fn test_apply_page_update_detects_stale_base() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        server
            .file_manager
            .write_page("doc", "# Doc\n\noriginal")
            .await
            .unwrap();

        let propose = server
            .tool_propose_page_update(&json!({
                "slug": "doc",
                "content": "# Doc\n\nproposto",
                "reason": "revisar"
            }))
            .await
            .unwrap();
        let id = extract_proposal_id(&propose[0].text);

        // alguém altera a página depois de a proposta ter sido criada
        server
            .file_manager
            .write_page("doc", "# Doc\n\nalterado por outro")
            .await
            .unwrap();

        // sem force → conflito
        let conflict = server
            .tool_apply_page_update(&json!({ "proposalId": id.as_str() }))
            .await;
        assert!(conflict.is_err());
        assert!(conflict.unwrap_err().contains("Conflito"));

        // com force → aplica mesmo assim
        let forced = server
            .tool_apply_page_update(&json!({ "proposalId": id.as_str(), "force": true }))
            .await
            .unwrap();
        assert!(forced[0].text.contains("force"));
        assert!(
            server
                .file_manager
                .read_page("doc")
                .await
                .unwrap()
                .contains("proposto")
        );
    }

    #[tokio::test]
    async fn test_apply_page_update_rejects_already_applied() {
        let dir = TempDir::new().unwrap();
        let server = make_server(dir.path()).await;

        let propose = server
            .tool_propose_page_update(&json!({
                "slug": "unica",
                "content": "# Única",
                "reason": "primeira aplicação"
            }))
            .await
            .unwrap();
        let id = extract_proposal_id(&propose[0].text);

        server
            .tool_apply_page_update(&json!({ "proposalId": id.as_str() }))
            .await
            .unwrap();

        let second = server
            .tool_apply_page_update(&json!({ "proposalId": id.as_str() }))
            .await;
        assert!(second.is_err());
        assert!(second.unwrap_err().contains("já foi aplicada"));
    }

    #[tokio::test]
    async fn test_tool_wiki_graph_summary() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server.file_manager.write_page("home", "wiki://page/about").await.unwrap();
        server.file_manager.write_page("about", "sobre").await.unwrap();

        let response = server.tool_wiki_graph(&json!({})).await.unwrap();
        assert!(response[0].text.contains("# Grafo da Wiki"));
        assert!(response[0].text.contains("- Páginas (nós): 2"));
        assert!(response[0].text.contains("- Links (arestas): 1"));
    }

    #[tokio::test]
    async fn test_tool_wiki_graph_rejects_invalid_format() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        let result = server.tool_wiki_graph(&json!({ "format": "xml" })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_backlinks() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server.file_manager.write_page("home", "wiki://page/about").await.unwrap();
        server.file_manager.write_page("about", "sobre").await.unwrap();

        // aceita a URI completa
        let response = server
            .tool_backlinks(&json!({ "slug": "wiki://page/about" }))
            .await
            .unwrap();
        assert!(response[0].text.contains("Backlinks para `about`"));
        assert!(response[0].text.contains("`home`"));

        // página inexistente → erro
        assert!(server
            .tool_backlinks(&json!({ "slug": "nao-existe" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_tool_orphans() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server.file_manager.write_page("home", "wiki://page/about").await.unwrap();
        server.file_manager.write_page("about", "sobre").await.unwrap();
        server.file_manager.write_page("lonely", "sozinha").await.unwrap();

        let response = server.tool_orphans(&json!({})).await.unwrap();
        assert!(response[0].text.contains("`home`"));
        assert!(response[0].text.contains("`lonely`"));
        assert!(!response[0].text.contains("`about`"));
    }

    #[tokio::test]
    async fn test_tool_related_pages() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server
            .file_manager
            .write_page("home", "wiki://page/about")
            .await
            .unwrap();
        server.file_manager.write_page("about", "wiki://page/home").await.unwrap();

        let response = server
            .tool_related_pages(&json!({ "slug": "home" }))
            .await
            .unwrap();
        assert!(response[0].text.contains("`about`"));
        assert!(response[0].text.contains("bidirecional"));
    }

    #[tokio::test]
    async fn test_tool_link_suggestions() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;

        let words: Vec<String> = (0..40).map(|i| format!("palavra{i:03}")).collect();
        let shared = words.join(" ");
        server.file_manager.write_page("page-a", &shared).await.unwrap();
        server.file_manager.write_page("page-b", &shared).await.unwrap();

        let response = server
            .tool_link_suggestions(&json!({}))
            .await
            .unwrap();
        assert!(response[0].text.contains("Sugestões de Links"));
        assert!(response[0].text.contains("`page-a`"));
        assert!(response[0].text.contains("`page-b`"));
    }

    #[tokio::test]
    async fn test_tool_find_claims() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server
            .file_manager
            .write_page(
                "doc",
                "# Doc\n\n## Claims\n\n- A usa três escopos.\n  - Source: `wiki://page/x`\n  - Confidence: high\n  - Last verified: 2026-05-11",
            )
            .await
            .unwrap();

        let response = server.tool_find_claims(&json!({})).await.unwrap();
        assert!(response[0].text.contains("`doc`"));
        assert!(response[0].text.contains("A usa três escopos."));
        assert!(response[0].text.contains("Confidence: high"));
    }

    #[tokio::test]
    async fn test_tool_find_claims_without_source() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server
            .file_manager
            .write_page(
                "doc",
                "## Claims\n\n- Claim com fonte.\n  - Source: `raw://source/y`\n\n- Claim sem fonte.\n  - Confidence: low",
            )
            .await
            .unwrap();

        let response = server
            .tool_find_claims_without_source(&json!({}))
            .await
            .unwrap();
        assert!(response[0].text.contains("Claim sem fonte."));
        assert!(!response[0].text.contains("Claim com fonte."));
    }

    #[tokio::test]
    async fn test_tool_find_conflicting_claims() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server
            .file_manager
            .write_page(
                "page-a",
                "## Claims\n\n- O servico usa autenticacao via token jwt assinado.",
            )
            .await
            .unwrap();
        server
            .file_manager
            .write_page(
                "page-b",
                "## Claims\n\n- O servico usa autenticacao via token jwt assinado.",
            )
            .await
            .unwrap();

        let response = server
            .tool_find_conflicting_claims(&json!({ "minSimilarity": 0.5 }))
            .await
            .unwrap();
        assert!(response[0].text.contains("Candidatos a Conflito"));
        assert!(response[0].text.contains("`page-a`"));
        assert!(response[0].text.contains("`page-b`"));
    }

    #[tokio::test]
    async fn test_tool_verify_claim() {
        let dir = TempDir::new().unwrap();
        let server = make_test_server(dir.path().to_path_buf()).await;
        server
            .file_manager
            .write_page(
                "doc",
                "## Claims\n\n- Afirmacao a verificar.\n  - Source: `wiki://page/x`\n  - Last verified: 2026-01-01",
            )
            .await
            .unwrap();

        let response = server
            .tool_verify_claim(&json!({ "slug": "doc", "claimIndex": 1, "date": "2026-05-16" }))
            .await
            .unwrap();
        assert!(response[0].text.contains("verificado"));

        let content = server.file_manager.read_page("doc").await.unwrap();
        assert!(content.contains("Last verified: 2026-05-16"));
        assert!(!content.contains("2026-01-01"));

        // índice inválido → erro
        assert!(server
            .tool_verify_claim(&json!({ "slug": "doc", "claimIndex": 9 }))
            .await
            .is_err());
    }

    // ── Busca híbrida (BM25 + semântica) ──────────────────────────────────────

    fn semantic_test_cfg() -> SemanticConfig {
        SemanticConfig::from_values(
            "k".into(),
            "http://localhost/v1".into(),
            "fake-model".into(),
            2000,
        )
    }

    /// Embeda o corpo com o `FakeEmbedder` (determinístico) e insere no store —
    /// mesmos vetores que o provider do servidor produziria para a query.
    async fn populate_store(store: &VectorStore, slug: &str, body: &str, cfg: &SemanticConfig) {
        use crate::embeddings::{EmbeddingProvider, FakeEmbedder, chunk_page};
        use crate::vector_store::{ChunkVec, PageEmbeddings};

        let stripped = crate::frontmatter::strip_frontmatter(body);
        let chunks = chunk_page(body, cfg);
        let texts: Vec<String> = chunks
            .iter()
            .map(|c| stripped[c.start..c.end].to_string())
            .collect();
        let vectors = FakeEmbedder::new().embed(&texts).await.unwrap();
        let dim = vectors[0].len() as u32;
        let chunk_vecs = chunks
            .iter()
            .zip(vectors)
            .map(|(c, vector)| ChunkVec {
                index: c.index,
                start: c.start,
                end: c.end,
                vector,
            })
            .collect();
        store.upsert(PageEmbeddings {
            slug: slug.into(),
            dim,
            model: cfg.model.clone(),
            body_hash: "h".into(),
            chunks: chunk_vecs,
        });
    }

    /// Monta wiki + índice BM25 + store. `pages`: (slug, corpo, indexar_no_store).
    /// Todas as páginas vão para o BM25; só as marcadas vão para o store.
    async fn semantic_parts(
        root: &Path,
        pages: &[(&str, &str, bool)],
    ) -> (
        Arc<WikiFileManager>,
        Arc<WikiSearchEngine>,
        Arc<VectorStore>,
        SemanticConfig,
    ) {
        let fm = Arc::new(WikiFileManager::new(Some(root.to_path_buf())));
        fm.init().await.unwrap();
        let engine = Arc::new(WikiSearchEngine::new(root.join(".advwiki/index")).unwrap());
        let store = Arc::new(VectorStore::new());
        let cfg = semantic_test_cfg();

        for (slug, body, in_store) in pages {
            fm.write_page(slug, body).await.unwrap();
            engine
                .index_document(
                    DocumentKind::Page,
                    &format!("wiki://page/{slug}"),
                    &slug.replace('-', " "),
                    body,
                    1000,
                )
                .unwrap();
            if *in_store {
                populate_store(&store, slug, body, &cfg).await;
            }
        }
        (fm, engine, store, cfg)
    }

    // Página "lexical" casa o termo `qwxz` (BM25); a query tem composição de
    // bytes próxima de "semantico" (muitos 'a'/'s') → cosseno favorece a segunda.
    const PAGE_LEXICAL: (&str, &str, bool) = ("lexical", "qwxz qwxz qwxz qwxz", true);
    const PAGE_SEMANTIC: (&str, &str, bool) =
        ("semantico", "asa asa asa asa asa asa asa asa asa asa", true);
    const HYBRID_QUERY: &str = "qwxz aaaaaaaa ssssssss";

    #[tokio::test]
    async fn test_query_semantic_mode_ranks_by_meaning() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, store, cfg) =
            semantic_parts(dir.path(), &[PAGE_LEXICAL, PAGE_SEMANTIC]).await;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(crate::embeddings::FakeEmbedder::new());
        let server = AdvWikiMcpServer::new(fm, engine, Some(store), Some(provider), Some(cfg));

        let resp = server
            .tool_query_wiki(&json!({ "question": HYBRID_QUERY, "mode": "semantic" }))
            .await
            .unwrap();
        let text = &resp[0].text;

        let pos_b = text
            .find("wiki://page/semantico")
            .expect("página semântica ausente");
        let pos_a = text.find("wiki://page/lexical");
        assert!(
            pos_a.map(|a| pos_b < a).unwrap_or(true),
            "semântica deveria rankear 'semantico' antes de 'lexical':\n{text}"
        );
        assert!(text.contains("via semântica"));
    }

    #[tokio::test]
    async fn test_query_bm25_mode_stays_lexical() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, store, cfg) =
            semantic_parts(dir.path(), &[PAGE_LEXICAL, PAGE_SEMANTIC]).await;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(crate::embeddings::FakeEmbedder::new());
        let server = AdvWikiMcpServer::new(fm, engine, Some(store), Some(provider), Some(cfg));

        let resp = server
            .tool_query_wiki(&json!({ "question": HYBRID_QUERY, "mode": "bm25" }))
            .await
            .unwrap();
        let text = &resp[0].text;

        // Só 'lexical' casa o termo; caminho clássico, sem anotação de origem.
        assert!(text.contains("wiki://page/lexical"));
        assert!(!text.contains("wiki://page/semantico"));
        assert!(!text.contains("via semântica") && !text.contains("via BM25"));
    }

    #[tokio::test]
    async fn test_query_auto_fuses_both_rankings() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, store, cfg) =
            semantic_parts(dir.path(), &[PAGE_LEXICAL, PAGE_SEMANTIC]).await;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(crate::embeddings::FakeEmbedder::new());
        let server = AdvWikiMcpServer::new(fm, engine, Some(store), Some(provider), Some(cfg));

        // mode omitido → default 'auto'.
        let resp = server
            .tool_query_wiki(&json!({ "question": HYBRID_QUERY }))
            .await
            .unwrap();
        let text = &resp[0].text;

        assert!(text.contains("wiki://page/lexical"), "BM25 deve contribuir:\n{text}");
        assert!(text.contains("wiki://page/semantico"), "semântica deve contribuir:\n{text}");
    }

    #[tokio::test]
    async fn test_query_falls_back_to_bm25_when_embed_fails() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, store, cfg) =
            semantic_parts(dir.path(), &[PAGE_LEXICAL, PAGE_SEMANTIC]).await;
        // Provider que sempre falha → a query não embeda → degrada pra BM25.
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(crate::embeddings::FakeEmbedder::failing(
            crate::embeddings::EmbedError::Transient("sem rede".into()),
        ));
        let server = AdvWikiMcpServer::new(fm, engine, Some(store), Some(provider), Some(cfg));

        let resp = server
            .tool_query_wiki(&json!({ "question": HYBRID_QUERY, "mode": "semantic" }))
            .await
            .expect("falha de embed deve degradar, não erro");
        let text = &resp[0].text;

        assert!(text.contains("wiki://page/lexical"));
        assert!(!text.contains("via semântica"), "não deveria haver trecho semântico:\n{text}");
    }

    #[tokio::test]
    async fn test_query_page_without_embedding_still_appears_via_bm25() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, store, cfg) = semantic_parts(
            dir.path(),
            &[
                PAGE_LEXICAL,
                ("noembed", "ccccc ccccc ccccc", false), // só no BM25
            ],
        )
        .await;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(crate::embeddings::FakeEmbedder::new());
        let server = AdvWikiMcpServer::new(fm, engine, Some(store), Some(provider), Some(cfg));

        let resp = server
            .tool_query_wiki(&json!({ "question": "ccccc", "mode": "auto" }))
            .await
            .unwrap();
        let text = &resp[0].text;
        assert!(
            text.contains("wiki://page/noembed"),
            "página sem embedding deve aparecer via BM25 (aditivo):\n{text}"
        );
    }

    #[tokio::test]
    async fn test_query_semantic_disabled_output_is_classic() {
        let dir = TempDir::new().unwrap();
        let (fm, engine, _store, _cfg) =
            semantic_parts(dir.path(), &[("lexical", "qwxz qwxz", false)]).await;
        // Semântica desligada (todos None) → caminho clássico, saída de sempre.
        let server = AdvWikiMcpServer::new(fm, engine, None, None, None);

        let resp = server
            .tool_query_wiki(&json!({ "question": "qwxz", "mode": "auto" }))
            .await
            .unwrap();
        let text = &resp[0].text;

        assert!(text.contains("# Resultados para:"));
        assert!(text.contains("wiki://page/lexical"));
        assert!(
            !text.contains("via semântica") && !text.contains("via BM25"),
            "caminho clássico não anota origem:\n{text}"
        );
    }
}
