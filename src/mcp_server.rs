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

use crate::search::{DocumentKind, WikiSearchEngine};
use crate::storage::WikiFileManager;

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

        // paginas dinamicas
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

        // raw resources
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

    // tools

    async fn handle_list_tools(&self, id: Option<Value>) -> JsonRpcResponse {
        let tools = vec![
            McpTool {
                name: "query_wiki".into(),
                description: Some("Busca textual na Wiki usando BM25. Retorna as páginas e raw sources relevantes.".into()),
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
            McpTool {
                name: "delete_page".into(),
                description: Some("Remove uma página da Wiki pelo slug.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Slug da página a ser removida"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Justificativa da remoção (registrada no log operacional)"
                        }
                    },
                    "required": ["slug"]
                }),
            },
            McpTool {
                name: "delete_raw_source".into(),
                description: Some("Remove uma raw source (conteúdo bruto + metadados) e atualiza o rawindex.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "sourceId": {
                            "type": "string",
                            "description": "Identificador da raw source a ser removida"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Justificativa da remoção (registrada no log operacional)"
                        }
                    },
                    "required": ["sourceId"]
                }),
            },
            McpTool {
                name: "list_pages_by_type".into(),
                description: Some("Lista páginas da Wiki que têm o campo 'type' do frontmatter igual ao valor informado.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "pageType": {
                            "type": "string",
                            "description": "Valor do campo 'type' a filtrar (ex: 'service', 'decision', 'pattern', 'bug', 'runbook')"
                        }
                    },
                    "required": ["pageType"]
                }),
            },
            McpTool {
                name: "list_pages_by_project".into(),
                description: Some("Lista páginas da Wiki que têm o campo 'project' do frontmatter igual ao valor informado.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Nome do projeto a filtrar (ex: 'auth-service', 'gateway')"
                        }
                    },
                    "required": ["project"]
                }),
            },
            McpTool {
                name: "list_pages_by_tag".into(),
                description: Some("Lista páginas da Wiki que contêm a tag informada no frontmatter.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "tag": {
                            "type": "string",
                            "description": "Tag a filtrar (ex: 'backend', 'api', 'mcp')"
                        }
                    },
                    "required": ["tag"]
                }),
            },
            McpTool {
                name: "find_pages_without_sources".into(),
                description: Some("Lista páginas da Wiki sem campo 'sources' no frontmatter (ou com sources vazio) — candidatas a revisão ou linkagem com raw sources.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "rebuild_wiki_index".into(),
                description: Some("Reconstrói a página de índice navegável da Wiki (wiki://page/index), agrupando todas as páginas por tipo e projeto conforme o frontmatter.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "propose_page_update".into(),
                description: Some("Propõe uma alteração de página sem gravá-la: salva uma proposta revisável e retorna o diff entre o conteúdo atual e o proposto. Use apply_page_update para aplicar.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Identificador da página alvo (ex: 'getting-started')"
                        },
                        "content": {
                            "type": "string",
                            "description": "Conteúdo Markdown completo proposto para a página"
                        },
                        "reason": {
                            "type": "string",
                            "description": "Justificativa da alteração (registrada na proposta e no log ao aplicar)"
                        }
                    },
                    "required": ["slug", "content", "reason"]
                }),
            },
            McpTool {
                name: "apply_page_update".into(),
                description: Some("Aplica uma proposta criada por propose_page_update. Revalida que a página não mudou desde a proposta antes de gravar.".into()),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "proposalId": {
                            "type": "string",
                            "description": "ID da proposta retornado por propose_page_update"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Se true, aplica mesmo que a página tenha mudado desde a proposta",
                            "default": false
                        }
                    },
                    "required": ["proposalId"]
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
            "delete_page" => self.tool_delete_page(&arguments).await,
            "delete_raw_source" => self.tool_delete_raw_source(&arguments).await,
            "list_pages_by_type" => self.tool_list_pages_by_type(&arguments).await,
            "list_pages_by_project" => self.tool_list_pages_by_project(&arguments).await,
            "list_pages_by_tag" => self.tool_list_pages_by_tag(&arguments).await,
            "find_pages_without_sources" => self.tool_find_pages_without_sources(&arguments).await,
            "rebuild_wiki_index" => self.tool_rebuild_wiki_index(&arguments).await,
            "propose_page_update" => self.tool_propose_page_update(&arguments).await,
            "apply_page_update" => self.tool_apply_page_update(&arguments).await,
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

    // tool - update_page
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

        let (raw_content, is_new_page) = match mode {
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
        };

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let final_content = crate::frontmatter::update_date_fields(&raw_content, &today, is_new_page);

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

    // tool - propose_page_update
    async fn tool_propose_page_update(&self, args: &Value) -> Result<Vec<McpToolContent>, String> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: slug")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: content")?;

        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or("Missing required arg: reason")?;

        // Lê o estado atual da página (base da proposta).
        let base_content = self.file_manager.read_page(slug).await.ok();
        let base_page_exists = base_content.is_some();
        let base_for_diff = base_content.clone().unwrap_or_default();
        let base_hash = base_content
            .as_deref()
            .map(crate::change_plan::content_hash);

        // Aplica os campos de data agora, para que o diff revisado seja
        // exatamente o conteúdo que será gravado na aplicação.
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let proposed_content =
            crate::frontmatter::update_date_fields(content, &today, !base_page_exists);

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

        let report = crate::lint::run_lint(scope, &self.file_manager, &self.search_engine)
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

    // tool - rebuild_wiki_index
    async fn tool_rebuild_wiki_index(&self, _args: &Value) -> Result<Vec<McpToolContent>, String> {
        use std::collections::BTreeMap;

        struct PageMeta {
            slug: String,
            page_type: Option<String>,
            project: Option<String>,
            status: Option<String>,
        }

        let slugs = self
            .file_manager
            .list_pages()
            .await
            .map_err(|e| format!("List error: {e}"))?;

        let mut pages: Vec<PageMeta> = Vec::new();
        for slug in &slugs {
            if slug == "index" {
                continue;
            }
            let fm = match self.file_manager.read_page(slug).await {
                Ok(content) => crate::frontmatter::parse_frontmatter(&content),
                Err(_) => None,
            };
            pages.push(PageMeta {
                slug: slug.clone(),
                page_type: fm.as_ref().and_then(|f| f.page_type.clone()),
                project: fm.as_ref().and_then(|f| f.project.clone()),
                status: fm.as_ref().and_then(|f| f.status.clone()),
            });
        }
        pages.sort_by(|a, b| a.slug.cmp(&b.slug));

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Group by type (BTreeMap keeps alphabetical order)
        let mut by_type: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut untyped: Vec<usize> = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            match &page.page_type {
                Some(t) => by_type.entry(t.clone()).or_default().push(i),
                None => untyped.push(i),
            }
        }

        // Group by project
        let mut by_project: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, page) in pages.iter().enumerate() {
            if let Some(proj) = &page.project {
                by_project.entry(proj.clone()).or_default().push(i);
            }
        }

        let mut lines: Vec<String> = vec![
            "---".into(),
            "type: index".into(),
            format!("updated_at: \"{today}\""),
            "---".into(),
            String::new(),
            "# Índice da Wiki".into(),
            String::new(),
            format!("> Gerado automaticamente por `rebuild_wiki_index` em {today}. Não edite manualmente."),
            String::new(),
            "## Por Tipo".into(),
            String::new(),
        ];

        if by_type.is_empty() && untyped.is_empty() {
            lines.push("_Nenhuma página encontrada._".into());
            lines.push(String::new());
        } else {
            for (type_name, indices) in &by_type {
                lines.push(format!("### {type_name}"));
                lines.push(String::new());
                for &i in indices {
                    let page = &pages[i];
                    let mut meta = Vec::new();
                    if let Some(p) = &page.project { meta.push(format!("project: {p}")); }
                    if let Some(s) = &page.status  { meta.push(format!("status: {s}")); }
                    let suffix = if meta.is_empty() { String::new() } else { format!(" — {}", meta.join(", ")) };
                    lines.push(format!("- [`{}`](wiki://page/{}){suffix}", page.slug, page.slug));
                }
                lines.push(String::new());
            }
            if !untyped.is_empty() {
                lines.push("### (sem tipo)".into());
                lines.push(String::new());
                for &i in &untyped {
                    let page = &pages[i];
                    let mut meta = Vec::new();
                    if let Some(p) = &page.project { meta.push(format!("project: {p}")); }
                    if let Some(s) = &page.status  { meta.push(format!("status: {s}")); }
                    let suffix = if meta.is_empty() { String::new() } else { format!(" — {}", meta.join(", ")) };
                    lines.push(format!("- [`{}`](wiki://page/{}){suffix}", page.slug, page.slug));
                }
                lines.push(String::new());
            }
        }

        if !by_project.is_empty() {
            lines.push("## Por Projeto".into());
            lines.push(String::new());
            for (proj_name, indices) in &by_project {
                lines.push(format!("### {proj_name}"));
                lines.push(String::new());
                for &i in indices {
                    let page = &pages[i];
                    let mut meta = Vec::new();
                    if let Some(t) = &page.page_type { meta.push(format!("type: {t}")); }
                    if let Some(s) = &page.status    { meta.push(format!("status: {s}")); }
                    let suffix = if meta.is_empty() { String::new() } else { format!(" — {}", meta.join(", ")) };
                    lines.push(format!("- [`{}`](wiki://page/{}){suffix}", page.slug, page.slug));
                }
                lines.push(String::new());
            }
        }

        lines.push("---".into());
        lines.push(format!("_Total: {} páginas indexadas._", pages.len()));

        let content = lines.join("\n");

        self.file_manager
            .write_page("index", &content)
            .await
            .map_err(|e| format!("Write error: {e}"))?;

        let log_entry = format!("[rebuild_wiki_index] Índice reconstruído com {} páginas", pages.len());
        let _ = self.file_manager.append_to_log(&log_entry).await;

        Ok(vec![McpToolContent {
            content_type: "text".into(),
            text: format!(
                "Índice reconstruído com sucesso.\n- Páginas indexadas: {}\n- URI: `wiki://page/index`",
                pages.len()
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
    async fn test_rebuild_wiki_index_groups_by_type_and_project() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let file_manager = Arc::new(WikiFileManager::new(Some(root.clone())));
        file_manager.init().await.unwrap();

        let search_engine = Arc::new(WikiSearchEngine::new(root.join(".advwiki/index")).unwrap());

        // Create pages with frontmatter
        file_manager
            .write_page("auth-service", "---\ntype: service\nproject: auth\nstatus: active\n---\n# Auth")
            .await
            .unwrap();
        file_manager
            .write_page("auth-adr-001", "---\ntype: decision\nproject: auth\nstatus: accepted\n---\n# ADR 001")
            .await
            .unwrap();
        file_manager
            .write_page("no-frontmatter", "# Plain page without frontmatter")
            .await
            .unwrap();

        let server = AdvWikiMcpServer::new(file_manager.clone(), search_engine);
        let result = server
            .tool_rebuild_wiki_index(&json!({}))
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].text.contains("3"));
        assert!(result[0].text.contains("wiki://page/index"));

        // Verify the written index page
        let index_content = file_manager.read_page("index").await.unwrap();
        assert!(index_content.contains("### service"));
        assert!(index_content.contains("### decision"));
        assert!(index_content.contains("### (sem tipo)"));
        assert!(index_content.contains("## Por Projeto"));
        assert!(index_content.contains("### auth"));
        assert!(index_content.contains("`auth-service`"));
        assert!(index_content.contains("`no-frontmatter`"));
        // The index page itself must not list itself
        assert!(!index_content.contains("`index`"));
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

        let server = AdvWikiMcpServer::new(file_manager, search_engine);
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
        AdvWikiMcpServer::new(file_manager, search_engine)
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
}
