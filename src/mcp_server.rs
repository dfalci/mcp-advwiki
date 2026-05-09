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

use crate::search::WikiSearchEngine;
use crate::storage::WikiFileManager;

// ── Tipos JSON-RPC ──────────────────────────────────────────────────────────

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

// ── Tipos MCP ───────────────────────────────────────────────────────────────
//
// Nomes em camelCase são intencionais — seguem a especificação JSON do MCP.

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

// ── Negociação de Protocolo ─────────────────────────────────────────────────

/// Versões de protocolo MCP suportadas, em ordem de preferência (mais recente primeiro).
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2024-11-05"];

/// Negocia a versão do protocolo MCP com o cliente.
///
/// Se o cliente solicitar uma versão suportada, ela é aceita.
/// Caso contrário, retorna a versão mais recente suportada.
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

// ── Servidor MCP ────────────────────────────────────────────────────────────

pub struct AdvWikiMcpServer {
    file_manager: Arc<WikiFileManager>,
    search_engine: Arc<WikiSearchEngine>,
}

impl AdvWikiMcpServer {
    pub fn new(
        file_manager: Arc<WikiFileManager>,
        search_engine: Arc<WikiSearchEngine>,
    ) -> Self {
        Self {
            file_manager,
            search_engine,
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

    /// Inicia o loop principal do servidor MCP sobre stdin/stdout.
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

    /// Roteia uma requisição JSON-RPC para o handler apropriado.
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

    // ── Initialize ──────────────────────────────────────────────────────────

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
                "version": "0.1.0"
            }),
        };

        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    // ── Resources ───────────────────────────────────────────────────────────

    async fn handle_list_resources(&self, id: Option<Value>) -> JsonRpcResponse {
        let mut resources = Vec::new();

        // Recursos fixos
        resources.push(McpResource {
            uri: "wiki://log".into(),
            name: "Log Operacional".into(),
            description: Some("Registro de operações da Wiki".into()),
            mimeType: Some("text/markdown".into()),
        });

        resources.push(McpResource {
            uri: "wiki://index".into(),
            name: "Índice Principal".into(),
            description: Some("Índice de raw sources (rawindex.md)".into()),
            mimeType: Some("text/markdown".into()),
        });

        // Páginas dinâmicas
        match self.file_manager.list_pages().await {
            Ok(slugs) => {
                for slug in slugs {
                    resources.push(McpResource {
                        uri: format!("wiki://page/{slug}"),
                        name: slug.replace('-', " "),
                        description: Some(format!("Página da Wiki: {}", slug)),
                        mimeType: Some("text/markdown".into()),
                    });
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Falha ao listar páginas para resources");
            }
        }

        // Raw sources
        match self.file_manager.list_raw_sources().await {
            Ok(ids) => {
                for source_id in ids {
                    resources.push(McpResource {
                        uri: format!("raw://source/{source_id}"),
                        name: format!("Raw Source: {source_id}"),
                        description: Some(format!("Conteúdo bruto: {source_id}")),
                        mimeType: Some("text/plain".into()),
                    });
                    resources.push(McpResource {
                        uri: format!("raw://sourcemetadata/{source_id}"),
                        name: format!("Metadados: {source_id}"),
                        description: Some(format!("Metadados JSON da source: {source_id}")),
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

    // ── Tools ───────────────────────────────────────────────────────────────

    async fn handle_list_tools(&self, id: Option<Value>) -> JsonRpcResponse {
        let tools = vec![
            McpTool {
                name: "query_wiki".into(),
                description: Some("Busca textual na Wiki usando BM25. Retorna páginas e raw sources relevantes.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "Termos de busca (ex: 'rust memory safety')"
                        },
                        "includeRawReferences": {
                            "type": "boolean",
                            "description": "Se true, inclui raw sources nos resultados",
                            "default": false
                        },
                        "maxPages": {
                            "type": "integer",
                            "description": "Número máximo de resultados",
                            "default": 10,
                            "minimum": 1,
                            "maximum": 50
                        }
                    },
                    "required": ["question"]
                }),
            },
            McpTool {
                name: "update_page".into(),
                description: Some("Cria ou atualiza uma página da Wiki. Suporta modo 'overwrite' (substitui todo o conteúdo) ou 'append' (adiciona ao final).".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Identificador único da página (ex: 'getting-started')"
                        },
                        "mode": {
                            "type": "string",
                            "description": "Modo de escrita: 'overwrite' ou 'append'",
                            "enum": ["overwrite", "append"]
                        },
                        "content": {
                            "type": "string",
                            "description": "Conteúdo em Markdown da página"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Justificativa da alteração (registrada no log operacional)"
                        }
                    },
                    "required": ["slug", "mode", "content"]
                }),
            },
            McpTool {
                name: "ingest_source".into(),
                description: Some("Ingere um arquivo externo como raw source na Wiki. Simula o download e salvamento.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "sourceUri": {
                            "type": "string",
                            "description": "URI do arquivo externo a ser ingerido"
                        },
                        "sourceType": {
                            "type": "string",
                            "description": "Tipo do conteúdo (ex: 'pdf', 'markdown', 'text')"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Se true, sobrescreve source existente com mesmo ID",
                            "default": false
                        }
                    },
                    "required": ["sourceUri", "sourceType"]
                }),
            },
            McpTool {
                name: "ingest_extracted_content".into(),
                description: Some("Salva texto extraído diretamente como raw source na Wiki.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "logicalUri": {
                            "type": "string",
                            "description": "URI lógica de destino (ex: 'raw://source/my-doc')"
                        },
                        "sourceType": {
                            "type": "string",
                            "description": "Tipo do conteúdo original (ex: 'pdf', 'markdown')"
                        },
                        "title": {
                            "type": "string",
                            "description": "Título descritivo do conteúdo"
                        },
                        "content": {
                            "type": "string",
                            "description": "Conteúdo textual extraído"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Se true, sobrescreve source existente",
                            "default": false
                        }
                    },
                    "required": ["logicalUri", "sourceType", "title", "content"]
                }),
            },
            McpTool {
                name: "lint_wiki".into(),
                description: Some("Executa validação estrutural da Wiki e retorna um relatório.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Escopo da validação: 'all' ou 'quick'",
                            "enum": ["all", "quick"]
                        }
                    },
                    "required": ["scope"]
                }),
            },
            McpTool {
                name: "read_knowledge_uri".into(),
                description: Some("Lê o conteúdo de qualquer URI lógica da Wiki (páginas, raw sources, metadados, log, índice).".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "uri": {
                            "type": "string",
                            "description": "URI lógica a ser lida (ex: wiki://page/home, raw://source/abc, wiki://log)"
                        }
                    },
                    "required": ["uri"]
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
            "ingest_source" => self.tool_ingest_source(&arguments).await,
            "ingest_extracted_content" => self.tool_ingest_extracted(&arguments).await,
            "lint_wiki" => self.tool_lint_wiki(&arguments).await,
            "read_knowledge_uri" => self.tool_read_knowledge_uri(&arguments).await,
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

    // ── Tool: query_wiki ────────────────────────────────────────────────────

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

        let mut results = self
            .search_engine
            .search(question, max_pages)
            .map_err(|e| format!("Search error: {e}"))?;

        // Filtra raw sources se includeRawReferences for false
        if !include_raw {
            results.retain(|r| !r.uri.starts_with("raw://"));
        }

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

    // ── Tool: update_page ───────────────────────────────────────────────────

    async fn tool_update_page(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
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

        let final_content = match mode {
            "overwrite" => content.to_string(),
            "append" => {
                // Lê conteúdo existente e concatena
                match self.file_manager.read_page(slug).await {
                    Ok(existing) => format!("{existing}\n\n{content}"),
                    Err(_) => content.to_string(),
                }
            }
            _ => return Err(format!("Invalid mode: {mode}. Use 'overwrite' or 'append'")),
        };

        self.file_manager
            .write_page(slug, &final_content)
            .await
            .map_err(|e| format!("Write error: {e}"))?;

        // Registra no log se houver rationale
        if let Some(reason) = rationale {
            let log_entry = format!("[{mode}] `{slug}`: {reason}");
            let _ = self.file_manager.append_to_log(&log_entry).await;
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!("Página `{slug}` atualizada com sucesso (modo: {mode})."),
        }])
    }

    // ── Tool: ingest_source ─────────────────────────────────────────────────

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

    // ── Tool: ingest_extracted_content ──────────────────────────────────────

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

        // Extrai source_id da logicalUri
        let source_id = logical_uri
            .strip_prefix("raw://source/")
            .unwrap_or(logical_uri)
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

    // ── Tool: lint_wiki ─────────────────────────────────────────────────────

    async fn tool_lint_wiki(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: scope")?;

        let mut report = Vec::new();
        report.push(format!("# Relatório de Validação (scope: {scope})\n"));

        // Conta páginas
        let pages = self
            .file_manager
            .list_pages()
            .await
            .map_err(|e| format!("Lint error: {e}"))?;

        report.push(format!("- Páginas encontradas: {}", pages.len()));

        // Conta raw sources
        let sources = self
            .file_manager
            .list_raw_sources()
            .await
            .map_err(|e| format!("Lint error: {e}"))?;

        report.push(format!("- Raw sources: {}", sources.len()));

        // Conta documentos no índice
        let doc_count = self
            .search_engine
            .doc_count()
            .map_err(|e| format!("Lint error: {e}"))?;

        report.push(format!("- Documentos no índice: {doc_count}"));

        // Verifica consistência (páginas + raw sources vs índice)
        let expected_count = (pages.len() + sources.len()) as u64;
        if expected_count != doc_count {
            report.push(format!(
                "- ⚠️ Inconsistência: {} documentos no disco ({} páginas + {} sources) vs {} no índice",
                expected_count,
                pages.len(),
                sources.len(),
                doc_count
            ));
        } else {
            report.push("- ✅ Índice consistente com o disco".to_string());
        }

        report.push("\n## Páginas\n".into());
        if pages.is_empty() {
            report.push("(nenhuma)".into());
        } else {
            for slug in &pages {
                report.push(format!("- `{slug}`"));
            }
        }

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: report.join("\n"),
        }])
    }

    // ── Tool: read_knowledge_uri ──────────────────────────────────────────────

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
