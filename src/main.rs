mod mcp_server;
mod search;
mod storage;
mod watcher;

use std::error::Error;
use std::sync::Arc;

const HELP_BANNER: &str = r#"
╔══════════════════════════════════════════════════════╗
║                                                      ║
║    ███╗   ███╗ ██████╗██████╗                         ║
║    ████╗ ████║██╔════╝██╔══██╗   █████╗               ║
║    ██╔████╔██║██║     ██████╔╝  ██╔══██╗              ║
║    ██║╚██╔╝██║██║     ██╔═══╝   ██║  ██║              ║
║    ██║ ╚═╝ ██║╚██████╗██║       ╚█████╔╝              ║
║    ╚═╝     ╚═╝ ╚═════╝╚═╝        ╚════╝               ║
║                                                      ║
║    █████╗ ██████╗ ██╗   ██╗██╗    ██╗██╗██╗  ██╗██╗   ║
║   ██╔══██╗██╔══██╗██║   ██║██║    ██║██║██║ ██╔╝██║   ║
║   ███████║██║  ██║██║   ██║██║ █╗ ██║██║█████╔╝ ██║   ║
║   ██╔══██║██║  ██║╚██╗ ██╔╝██║███╗██║██║██╔═██╗ ██║   ║
║   ██║  ██║██████╔╝ ╚████╔╝ ╚███╔███╔╝██║██║  ██╗██║   ║
║   ╚═╝  ╚═╝╚═════╝   ╚═══╝   ╚══╝╚══╝ ╚═╝╚═╝  ╚═╝╚═╝   ║
║                                                      ║
║     Servidor MCP — Wiki Local com Busca Full-Text     ║
║              Desenvolvido por Daniel Falci            ║
║                                                      ║
╚══════════════════════════════════════════════════════╝

Uso:
  mcp-advwiki              Inicia o servidor MCP (escuta stdin/stdout)
  mcp-advwiki -h, --help   Mostra esta ajuda

Configuração para Claude Desktop (claude_desktop_config.json):
  {
    "mcpServers": {
      "advwiki": {
        "command": "mcp-advwiki",
        "args": []
      }
    }
  }
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Verifica flag de ajuda antes de qualquer inicialização
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP_BANNER}");
        return Ok(());
    }

    // ── Configura tracing para stderr ────────────────────────────────────
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("info")
        .init();

    tracing::info!("Iniciando o Servidor MCP AdvWiki...");

    // 0. Inicializa o gerenciador de arquivos da Wiki
    let wiki = Arc::new(storage::WikiFileManager::new(None));
    wiki.init().await?;
    tracing::info!(
        wiki_dir = %wiki.wiki_dir().display(),
        "Wiki inicializada"
    );

    // 1. Inicializa o motor de busca (Tantivy BM25)
    let index_path = wiki.wiki_dir().join("index");
    let search_engine = Arc::new(
        search::WikiSearchEngine::new(index_path)?,
    );
    tracing::info!(
        index = %search_engine.index_path().display(),
        "Índice de busca inicializado"
    );

    // 1.5 Reindexa conteúdo já existente no disco
    rebuild_index(&wiki, &search_engine).await?;

    // 2. Inicializa o File Watcher (reatividade)
    let (mut event_rx, _watcher) = watcher::WikiWatcher::start(
        wiki.root().to_path_buf(),
        wiki.wiki_dir().to_path_buf(),
    )?;
    tracing::info!("File watcher iniciado");

    // 3. Task de consumo de eventos do watcher → atualização do índice
    let search_clone = search_engine.clone();
    let wiki_clone = wiki.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            handle_wiki_event(&search_clone, &wiki_clone, event).await;
        }
    });

    // 4. Inicia o servidor MCP (bloqueante — escuta stdin/stdout)
    let server = mcp_server::AdvWikiMcpServer::new(wiki, search_engine);
    server.run().await?;

    Ok(())
}

/// Reage a um evento do sistema de arquivos atualizando o índice Tantivy.
async fn handle_wiki_event(
    engine: &search::WikiSearchEngine,
    wiki: &storage::WikiFileManager,
    event: watcher::WikiEvent,
) {
    use watcher::WikiEvent;

    match event {
        WikiEvent::PageCreated { slug } | WikiEvent::PageUpdated { slug } => {
            let uri = format!("wiki://page/{slug}");
            match wiki.read_page(&slug).await {
                Ok(content) => {
                    // O título é o slug com hífens substituídos por espaços
                    let title = slug.replace('-', " ");
                    let now = chrono::Utc::now().timestamp();
                    if let Err(e) = engine.index_document(&uri, &title, &content, now) {
                        tracing::error!(%slug, error = %e, "Falha ao indexar página");
                    } else {
                        tracing::debug!(%slug, "Página indexada");
                    }
                }
                Err(e) => {
                    tracing::error!(%slug, error = %e, "Falha ao ler página para indexação");
                }
            }
        }

        WikiEvent::PageDeleted { slug } => {
            let uri = format!("wiki://page/{slug}");
            if let Err(e) = engine.delete_document(&uri) {
                tracing::error!(%slug, error = %e, "Falha ao remover página do índice");
            } else {
                tracing::debug!(%slug, "Página removida do índice");
            }
        }

        WikiEvent::RawSourceUpdated { source_id } => {
            let uri = format!("raw://source/{source_id}");
            match wiki.read_raw_source(&source_id).await {
                Ok(content) => {
                    let now = chrono::Utc::now().timestamp();
                    if let Err(e) = engine.index_document(&uri, &source_id, &content, now) {
                        tracing::error!(%source_id, error = %e, "Falha ao indexar raw source");
                    } else {
                        tracing::debug!(%source_id, "Raw source indexada");
                    }
                }
                Err(e) => {
                    tracing::error!(%source_id, error = %e, "Falha ao ler raw source para indexação");
                }
            }
        }

        WikiEvent::RawSourceDeleted { source_id } => {
            let uri = format!("raw://source/{source_id}");
            if let Err(e) = engine.delete_document(&uri) {
                tracing::error!(%source_id, error = %e, "Falha ao remover raw source do índice");
            } else {
                tracing::debug!(%source_id, "Raw source removida do índice");
            }
        }

        WikiEvent::IndexChanged => {
            tracing::info!("rawindex.md alterado — reindexação completa pode ser necessária");
        }

        WikiEvent::LogChanged => {
            tracing::debug!("Log operacional alterado (não requer reindexação)");
        }

        WikiEvent::Unknown(desc) => {
            tracing::warn!(event = %desc, "Evento desconhecido recebido");
        }
    }
}

/// Reconstrói o índice Tantivy a partir de todo o conteúdo já existente
/// no disco (páginas e raw sources). Chamado uma vez na inicialização.
async fn rebuild_index(
    wiki: &storage::WikiFileManager,
    engine: &search::WikiSearchEngine,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut docs: Vec<(String, String, String, i64)> = Vec::new();

    // Páginas da Wiki
    match wiki.list_pages().await {
        Ok(slugs) => {
            for slug in &slugs {
                match wiki.read_page(slug).await {
                    Ok(content) => {
                        let uri = format!("wiki://page/{slug}");
                        let title = slug.replace('-', " ");
                        docs.push((uri, title, content, now));
                    }
                    Err(e) => {
                        tracing::warn!(%slug, error = %e, "Falha ao ler página durante rebuild");
                    }
                }
            }
            tracing::info!(pages = %slugs.len(), "Páginas varridas para rebuild");
        }
        Err(e) => {
            tracing::error!(error = %e, "Falha ao listar páginas durante rebuild");
        }
    }

    // Raw sources
    match wiki.list_raw_sources().await {
        Ok(source_ids) => {
            for source_id in &source_ids {
                match wiki.read_raw_source(source_id).await {
                    Ok(content) => {
                        let uri = format!("raw://source/{source_id}");
                        docs.push((uri, source_id.clone(), content, now));
                    }
                    Err(e) => {
                        tracing::warn!(%source_id, error = %e, "Falha ao ler raw source durante rebuild");
                    }
                }
            }
            tracing::info!(sources = %source_ids.len(), "Raw sources varridas para rebuild");
        }
        Err(e) => {
            tracing::error!(error = %e, "Falha ao listar raw sources durante rebuild");
        }
    }

    if docs.is_empty() {
        tracing::info!("Nenhum documento encontrado no disco para rebuild");
        return Ok(());
    }

    let count = engine.index_bulk(&docs)?;
    tracing::info!(count = %count, total_docs = %docs.len(), "Rebuild inicial do índice concluído");

    Ok(())
}