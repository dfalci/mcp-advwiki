mod change_plan;
mod claims;
mod frontmatter;
mod graph;
mod lint;
mod mcp_server;
mod migration;
mod search;
mod storage;
mod watcher;

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Intervalo entre tentativas de promoção quando a instância está em modo Secondary.
/// Trade-off: muito curto = log/CPU desnecessários; muito longo = atraso pra
/// assumir o índice depois que a primary cai.
const SECONDARY_PROMOTION_POLL_INTERVAL: Duration = Duration::from_secs(5);

const HELP_BANNER: &str = concat!(
    r#"
╔═══════════════════════════════════════════════════════╗
║                                                       ║
║    ███╗   ███╗ ██████╗██████╗                         ║
║    ████╗ ████║██╔════╝██╔══██╗   █████╗               ║
║    ██╔████╔██║██║     ██████╔╝  ██╔══██╗              ║
║    ██║╚██╔╝██║██║     ██╔═══╝   ██║  ██║              ║
║    ██║ ╚═╝ ██║╚██████╗██║       ╚█████╔╝              ║
║    ╚═╝     ╚═╝ ╚═════╝╚═╝        ╚════╝               ║
║                                                       ║
║    █████╗ ██████╗ ██╗   ██╗██╗    ██╗██╗██╗  ██╗██╗   ║
║   ██╔══██╗██╔══██╗██║   ██║██║    ██║██║██║ ██╔╝██║   ║
║   ███████║██║  ██║██║   ██║██║ █╗ ██║██║█████╔╝ ██║   ║
║   ██╔══██║██║  ██║╚██╗ ██╔╝██║███╗██║██║██╔═██╗ ██║   ║
║   ██║  ██║██████╔╝ ╚████╔╝ ╚███╔███╔╝██║██║  ██╗██║   ║
║   ╚═╝  ╚═╝╚═════╝   ╚═══╝   ╚══╝╚══╝ ╚═╝╚═╝  ╚═╝╚═╝   ║
║                                                       ║
║     Server MCP — Wiki Local Full-Text Search          ║
║           Developed by Daniel Falci                   ║
║                                                       ║
╚═══════════════════════════════════════════════════════╝

  mcp-advwiki v"#,
    env!("CARGO_PKG_VERSION"),
    r#"

Uso:
  mcp-advwiki [--root <PATH>]        start the mcpserver (listen stdin/stdout)
  mcp-advwiki -h, --help             show this help message

Opcoes:
  -r, --root <PATH>  basedir from where the files
                     `.advwiki/`, `.advwikilog.md` e `rawindex.md`
                     will be read/created.
                     defaults execution dir.
  -h, --help         show this help message

Claude Desktop configuration (claude_desktop_config.json):
  {
    "mcpServers": {
      "advwiki": {
        "command": "mcp-advwiki",
        "args": ["--root", "/path/to/wiki/root"]
      }
    }
  }
"#
);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CliOptions {
    root: Option<PathBuf>,
    show_help: bool,
}

fn parse_cli_args(args: &[String]) -> Result<CliOptions, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(CliOptions {
            show_help: true,
            ..CliOptions::default()
        });
    }

    let mut options = CliOptions::default();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];

        match arg.as_str() {
            "-r" | "--root" => {
                let next = args
                    .get(index + 1)
                    .ok_or_else(|| format!("O parametro {arg} requer um caminho."))?;

                if next.starts_with('-') {
                    return Err(format!(
                        "O parametro {arg} requer um caminho valido. Recebido: {next}"
                    ));
                }

                set_root_option(&mut options, next)?;
                index += 2;
            }
            _ if arg.starts_with("--root=") => {
                let value = &arg["--root=".len()..];
                set_root_option(&mut options, value)?;
                index += 1;
            }
            _ if arg.starts_with("-r=") => {
                let value = &arg["-r=".len()..];
                set_root_option(&mut options, value)?;
                index += 1;
            }
            _ => {
                return Err(format!("Parametro desconhecido: {arg}"));
            }
        }
    }

    Ok(options)
}

fn set_root_option(options: &mut CliOptions, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("O parametro --root nao pode receber um caminho vazio.".to_string());
    }

    if options.root.is_some() {
        return Err("O parametro --root foi informado mais de uma vez.".to_string());
    }

    options.root = Some(PathBuf::from(value));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse_cli_args(&raw_args) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}\n\nUse -h ou --help para ver as opcoes disponiveis.");
            std::process::exit(2);
        }
    };

    if cli.show_help {
        println!("{HELP_BANNER}");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("info")
        .init();

    tracing::info!("Iniciando o Servidor MCP AdvWiki...");

    //inicializa o gerenciador de arquivos
    let wiki = Arc::new(storage::WikiFileManager::new(cli.root));
    wiki.init().await?;
    tracing::info!(
        root = %wiki.root().display(),
        wiki_dir = %wiki.wiki_dir().display(),
        "Wiki inicializada"
    );

    // Migrações de schema (idempotentes — quando a wiki já está atualizada, é no-op).
    // Roda ANTES de qualquer indexação para que o índice já reflita o formato novo.
    match migration::run_if_needed(&wiki).await {
        Ok(report) if !report.skipped => {
            tracing::info!(
                from = report.from_version,
                to = report.to_version,
                pages = report.pages_changed,
                links = report.links_converted,
                dry_run = report.dry_run,
                "Migração de schema concluída"
            );
        }
        Ok(_) => {}
        Err(e) => {
            // Migração falhou — loga e segue. Wiki pré-existente continua funcionando
            // graças ao parser lenient (que reconhece os dois formatos).
            tracing::error!(error = %e, "Migração de schema falhou — seguindo com formato legado");
        }
    }

    let index_path = wiki.wiki_dir().join("index");
    let search_engine = Arc::new(
        search::WikiSearchEngine::new(index_path)?,
    );
    tracing::info!(
        index = %search_engine.index_path().display(),
        "Índice de busca inicializado"
    );

    // Conforme o papel detectado pelo engine: a primary assume o índice
    // imediatamente; a secondary fica polling até a primary cair.
    if search_engine.is_primary() {
        let wiki_clone = wiki.clone();
        let engine_clone = search_engine.clone();
        tokio::spawn(async move {
            if let Err(e) = run_primary_role(wiki_clone, engine_clone).await {
                tracing::error!(error = %e, "Falha ao executar papel de primary");
            }
        });
    } else {
        let wiki_clone = wiki.clone();
        let engine_clone = search_engine.clone();
        tokio::spawn(async move {
            run_secondary_with_failover(wiki_clone, engine_clone).await;
        });
    }

    //inicia o servidor MCP (blockado.. escuta stdin/stdout)
    let server = mcp_server::AdvWikiMcpServer::new(wiki, search_engine);
    server.run().await?;

    Ok(())
}

/// Executa as responsabilidades da instância primary: rebuild inicial do índice
/// e consumo de eventos do file watcher.
///
/// Roda enquanto o watcher mantiver o canal aberto. Se o watcher falhar ao
/// iniciar, retorna erro; se o canal fechar (ex: drop do watcher), retorna Ok.
async fn run_primary_role(
    wiki: Arc<storage::WikiFileManager>,
    engine: Arc<search::WikiSearchEngine>,
) -> anyhow::Result<()> {
    rebuild_index(&wiki, &engine).await?;

    let (mut event_rx, _watcher) = watcher::WikiWatcher::start(wiki.root().to_path_buf())?;
    tracing::info!("File watcher iniciado");

    while let Some(event) = event_rx.recv().await {
        handle_wiki_event(&engine, &wiki, event).await;
    }

    tracing::info!("Loop de eventos do watcher encerrado");
    Ok(())
}

/// Polling em background para que a instância Secondary assuma o papel de
/// Primary caso a primary atual caia (libera o writer lock do Tantivy).
///
/// Roda indefinidamente até a promoção ser bem-sucedida; após promover,
/// transfere o controle pra `run_primary_role`.
async fn run_secondary_with_failover(
    wiki: Arc<storage::WikiFileManager>,
    engine: Arc<search::WikiSearchEngine>,
) {
    tracing::info!(
        poll_interval_s = SECONDARY_PROMOTION_POLL_INTERVAL.as_secs(),
        "Modo secondary: aguardando para assumir o papel de primary"
    );

    loop {
        tokio::time::sleep(SECONDARY_PROMOTION_POLL_INTERVAL).await;

        if engine.try_promote_to_primary() {
            tracing::info!("Promovido a primary — assumindo controle do índice");
            if let Err(e) = run_primary_role(wiki, engine).await {
                tracing::error!(error = %e, "Falha ao executar papel de primary após promoção");
            }
            return;
        }

        tracing::debug!("Tentativa de promoção falhou; continua em modo secondary");
    }
}

#[cfg(test)]
mod tests {
    use super::{CliOptions, parse_cli_args};
    use std::path::PathBuf;

    #[test]
    fn parse_cli_defaults_to_current_directory_behavior() {
        let cli = parse_cli_args(&[]).expect("parse should succeed");
        assert_eq!(
            cli,
            CliOptions {
                root: None,
                show_help: false,
            }
        );
    }

    #[test]
    fn parse_cli_accepts_root_with_separate_value() {
        let cli = parse_cli_args(&["--root".into(), "C:\\repo".into()])
            .expect("parse should succeed");

        assert_eq!(cli.root, Some(PathBuf::from("C:\\repo")));
        assert!(!cli.show_help);
    }

    #[test]
    fn parse_cli_accepts_root_inline_value() {
        let cli = parse_cli_args(&["--root=C:\\repo".into()])
            .expect("parse should succeed");

        assert_eq!(cli.root, Some(PathBuf::from("C:\\repo")));
        assert!(!cli.show_help);
    }

    #[test]
    fn parse_cli_prioritizes_help() {
        let cli = parse_cli_args(&["--help".into(), "--root".into(), "C:\\repo".into()])
            .expect("parse should succeed");

        assert!(cli.show_help);
        assert_eq!(cli.root, None);
    }

    #[test]
    fn parse_cli_rejects_missing_root_value() {
        let err = parse_cli_args(&["--root".into()]).expect_err("parse should fail");
        assert!(err.contains("requer um caminho"));
    }

    #[test]
    fn parse_cli_rejects_duplicate_root() {
        let err = parse_cli_args(&[
            "--root".into(),
            "C:\\repo".into(),
            "-r".into(),
            "D:\\repo".into(),
        ])
        .expect_err("parse should fail");

        assert!(err.contains("mais de uma vez"));
    }
}

/// reage a um evento do sistema de arquivos atualizando o índice Tantivy.
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
                    let title = slug.replace('-', " ");
                    let now = chrono::Utc::now().timestamp();
                    let indexed_content = frontmatter::strip_frontmatter(&content);
                    if let Err(e) = engine.index_document(search::DocumentKind::Page, &uri, &title, indexed_content, now) {
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
                    if let Err(e) = engine.index_document(search::DocumentKind::Raw, &uri, &source_id, &content, now) {
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
            tracing::info!("rawindex.md alterado — iniciando rebuild completo do índice");
            if let Err(e) = rebuild_index(wiki, engine).await {
                tracing::error!(error = %e, "Falha ao reconstruir índice após alteração em rawindex.md");
            }
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
    let mut docs: Vec<(search::DocumentKind, String, String, String, i64)> = Vec::new();

    // páginas da Wiki
    match wiki.list_pages().await {
        Ok(slugs) => {
            for slug in &slugs {
                match wiki.read_page(slug).await {
                    Ok(content) => {
                        let uri = format!("wiki://page/{slug}");
                        let title = slug.replace('-', " ");
                        let indexed_content = frontmatter::strip_frontmatter(&content).to_string();
                        docs.push((search::DocumentKind::Page, uri, title, indexed_content, now));
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

    // raw sources
    match wiki.list_raw_sources().await {
        Ok(source_ids) => {
            for source_id in &source_ids {
                match wiki.read_raw_source(source_id).await {
                    Ok(content) => {
                        let uri = format!("raw://source/{source_id}");
                        docs.push((search::DocumentKind::Raw, uri, source_id.clone(), content, now));
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

    engine.clear()?;
    let count = engine.index_bulk(&docs)?;
    tracing::info!(count = %count, total_docs = %docs.len(), "Rebuild inicial do índice concluído");

    Ok(())
}