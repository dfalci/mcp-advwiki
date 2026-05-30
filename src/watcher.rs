// ── Módulo de File Watcher (Reatividade) ───────────────────────────────────
//
// Monitora o diretório físico da Wiki (`.advwiki/` + arquivos raiz) usando a
// crate `notify` e traduz eventos brutos do sistema de arquivos em eventos
// de domínio (`WikiEvent`), despachados via canal assíncrono `tokio::mpsc`.
//
// Arquitetura de bridge:
//
//   FS ──► notify::RecommendedWatcher ──► std::sync::mpsc ──► spawn_blocking
//                                         (thread interna)      │
//                                                         tradução + filtro
//                                                               │
//                                                         tokio::mpsc ──► consumidor
//                                                                          (Tantivy)

use anyhow::Context;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc as tokio_mpsc;


/// Eventos lógicos gerados a partir de alterações no sistema de arquivos da Wiki.
///
/// Cada variante carrega apenas a informação essencial para que o consumidor
/// (ex: motor de busca Tantivy) saiba o que indexar, reindexar ou remover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiEvent {
    /// Uma página (`.md`) foi criada em `.advwiki/pages/`.
    PageCreated {
        /// Slug da página (nome do arquivo sem `.md`).
        slug: String,
    },
    /// Uma página (`.md`) foi modificada em `.advwiki/pages/`.
    PageUpdated {
        slug: String,
    },
    /// Uma página (`.md`) foi removida de `.advwiki/pages/`.
    PageDeleted {
        slug: String,
    },
    /// O arquivo `rawindex.md` (índice principal) foi alterado.
    IndexChanged,
    /// O arquivo `.advwikilog.md` (log operacional) foi alterado.
    LogChanged,
    /// Uma raw source (arquivo em `sources/` ou `metadata/`) foi criada ou modificada.
    RawSourceUpdated {
        /// Identificador da raw source (nome do arquivo em `sources/`).
        source_id: String,
    },
    /// Uma raw source (arquivo em `sources/` ou `metadata/`) foi removida.
    RawSourceDeleted {
        source_id: String,
    },
    /// Evento não classificado — contém uma descrição para depuração.
    Unknown(String),
}


/// Serviço de monitoramento reativo do sistema de arquivos da Wiki.
///
/// # Uso típico
///
/// ```ignore
/// let (rx, watcher) = WikiWatcher::start(root, wiki_dir)?;
/// // consumir rx em outra task
/// // quando watcher é dropado, o monitoramento para
/// ```
pub struct WikiWatcher {
    /// O watcher nativo do `notify`. Mantido vivo para que o watch continue.
    /// Quando dropado, interrompe o monitoramento.
    _watcher: RecommendedWatcher,
    /// Handle da tarefa de bridge em background.
    _bg_task: tokio::task::JoinHandle<()>,
}

impl WikiWatcher {
    /// Inicia o monitoramento e retorna o receiver + o watcher.
    ///
    /// - `root`: diretório raiz do projeto (contém `.advwikilog.md` e `rawindex.md`).
    ///
    /// O watcher monitora:
    /// - `.advwiki/` recursivamente (cobre `pages/`, `sources/`, `metadata/`)
    /// - Arquivos na raiz: `.advwikilog.md`, `rawindex.md`
    pub fn start(
        root: PathBuf,
    ) -> anyhow::Result<(tokio_mpsc::UnboundedReceiver<WikiEvent>, Self)> {
        // Canonicaliza o root e deriva o wiki_dir antes de qualquer
        // operação, garantindo que `watcher.watch()` e a `bg_task` usem
        // os mesmos prefixos. Se `notify` emitir paths com prefixo
        // diferente, as comparações `starts_with` falhariam silenciosamente.
        let root = std::fs::canonicalize(&root)
            .context("Falha ao canonicalizar o diretório raiz da Wiki")?;
        let wiki_dir = root.join(".advwiki");
        let log_path = root.join(".advwikilog.md");
        let index_path = root.join("rawindex.md");

        // canal de domínio (assíncrono)
        let (domain_tx, domain_rx) = tokio_mpsc::unbounded_channel();

        // canal bruto (síncrono) para bridge com notify
        let (raw_tx, raw_rx) = std_mpsc::channel::<notify::Result<Event>>();

        // watcher nativo do notify
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                // O callback roda na thread interna do notify.
                // Encaminhamos para o canal síncrono; se o receiver fechou,
                // o erro é silenciado (shutdown em andamento).
                let _ = raw_tx.send(res);
            },
            notify::Config::default(),
        )
        .context("Falha ao criar o RecommendedWatcher do notify")?;

        // registra os caminhos a monitorar (canônicos)
        watcher
            .watch(&wiki_dir, RecursiveMode::Recursive)
            .with_context(|| format!("Falha ao monitorar: {}", wiki_dir.display()))?;

        // monitora arquivos individuais na raiz (não-recursivo para não capturar `target/`, `src/`, etc.)
        if log_path.exists() {
            watcher
                .watch(&log_path, RecursiveMode::NonRecursive)
                .with_context(|| format!("Falha ao monitorar: {}", log_path.display()))?;
        }
        if index_path.exists() {
            watcher
                .watch(&index_path, RecursiveMode::NonRecursive)
                .with_context(|| format!("Falha ao monitorar: {}", index_path.display()))?;
        }

        tracing::info!(
            root = %root.display(),
            wiki_dir = %wiki_dir.display(),
            "File watcher iniciado"
        );

        // tarefa de bridge em background
        //
        // `spawn_blocking` é usado porque `raw_rx.recv()` é bloqueante.
        // A tarefa lê eventos brutos, traduz para `WikiEvent` e envia pelo
        // canal de domínio. Sai quando o `raw_tx` é dropado (junto com o
        // `_watcher` no Drop do `WikiWatcher`).
        //
        // `root` e `wiki_dir` já estão canonicalizados aqui — movemos
        // clones para dentro da closure.
        let bg_root = root.clone();
        let bg_wiki_dir = wiki_dir.clone();

        let bg_task = tokio::task::spawn_blocking(move || {
            while let Ok(raw_result) = raw_rx.recv() {
                match raw_result {
                    Ok(event) => {
                        let domain_events =
                            translate_event(&event, &bg_root, &bg_wiki_dir);

                        for domain_event in domain_events {
                            // ignora erro de envio — significa que o receiver
                            // foi dropado (shutdown).
                            if domain_tx.send(domain_event).is_err() {
                                tracing::debug!("Canal de domínio fechado, watcher encerrando");
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Erro no notify watcher");
                    }
                }
            }

            tracing::debug!("Bridge do watcher encerrada (raw channel fechado)");
        });

        Ok((
            domain_rx,
            Self {
                _watcher: watcher,
                _bg_task: bg_task,
            },
        ))
    }
}

// tradução de eventos

/// Traduz um `notify::Event` bruto em zero ou mais `WikiEvent`s de domínio.
///
/// Filtra eventos irrelevantes (ex: arquivos temporários, diretórios,
/// modificações de metadata) e extrai slugs de páginas.
///
/// Usa comparação por caminhos brutos (não canonicalizados) para que eventos
/// `Remove` não sejam perdidos — `path.canonicalize()` falha quando o arquivo
/// já foi excluído.
fn classify_single_path_event(
    path: &Path,
    kind: &EventKind,
    root: &Path,
    wiki_dir: &Path,
) -> Option<WikiEvent> {
    let pages_dir = wiki_dir.join("pages");
    let sources_dir = wiki_dir.join("sources");
    let metadata_dir = wiki_dir.join("metadata");
    let proposals_dir = wiki_dir.join("proposals");
    let expected_log = root.join(".advwikilog.md");
    let expected_index = root.join("rawindex.md");

    if path_is_in(path, &pages_dir) {
        return extract_slug(path, &pages_dir).map(|slug| match kind {
            EventKind::Create(_) => WikiEvent::PageCreated { slug },
            // Renomeações single-path: a plataforma pode emitir `From`/`To` em
            // eventos separados (1 path cada). `From` = origem que sumiu (delete);
            // `To` = destino que apareceu (create). Sem estes arms, ambos cairiam
            // em `Modify(_)` → PageUpdated, deixando a entrada antiga órfã no índice.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => WikiEvent::PageDeleted { slug },
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => WikiEvent::PageCreated { slug },
            EventKind::Modify(_) => WikiEvent::PageUpdated { slug },
            EventKind::Remove(_) => WikiEvent::PageDeleted { slug },
            _ => WikiEvent::Unknown(format!(
                "Evento inesperado em página: {:?} → {}",
                kind,
                path.display()
            )),
        });
    }

    if path_equals(path, &expected_log) {
        return Some(WikiEvent::LogChanged);
    }
    if path_equals(path, &expected_index) {
        return Some(WikiEvent::IndexChanged);
    }

    if path_is_in(path, &sources_dir) || path_is_in(path, &metadata_dir) {
        return Some(match extract_source_id(path, &sources_dir, &metadata_dir) {
            Some(source_id) => match kind {
                // Mesma lógica das páginas: `From` single-path é remoção.
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                    WikiEvent::RawSourceDeleted { source_id }
                }
                EventKind::Create(_) | EventKind::Modify(_) => {
                    WikiEvent::RawSourceUpdated { source_id }
                }
                EventKind::Remove(_) => WikiEvent::RawSourceDeleted { source_id },
                _ => WikiEvent::Unknown(format!(
                    "Evento inesperado em raw source: {:?} → {}",
                    kind,
                    path.display()
                )),
            },
            None => WikiEvent::Unknown(format!(
                "Não foi possível extrair source_id de: {}",
                path.display()
            )),
        });
    }

    // Propostas de alteração não são indexáveis — ignoramos seus eventos.
    if path_is_in(path, &proposals_dir) {
        return None;
    }

    if path_is_in(path, wiki_dir) {
        return Some(WikiEvent::Unknown(format!(
            "Alteração não classificada dentro da wiki: {} ({:?})",
            path.display(),
            kind
        )));
    }

    None
}

fn translate_event(
    event: &Event,
    root: &Path,
    wiki_dir: &Path,
) -> Vec<WikiEvent> {
    // ignora eventos de Access (alta frequência, sem efeito no índice)
    if matches!(event.kind, EventKind::Access(_)) {
        return Vec::new();
    }

    if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            | EventKind::Modify(ModifyKind::Name(RenameMode::From))
            | EventKind::Modify(ModifyKind::Name(RenameMode::To))
            | EventKind::Modify(ModifyKind::Name(RenameMode::Any))
            | EventKind::Modify(ModifyKind::Name(RenameMode::Other))
    ) && event.paths.len() >= 2
    {
        let mut domain_events = Vec::new();
        if let Some(old_event) = classify_single_path_event(&event.paths[0], &EventKind::Remove(notify::event::RemoveKind::Any), root, wiki_dir) {
            domain_events.push(old_event);
        }
        if let Some(new_event) = classify_single_path_event(&event.paths[1], &EventKind::Create(notify::event::CreateKind::Any), root, wiki_dir) {
            domain_events.push(new_event);
        }
        return domain_events;
    }

    event
        .paths
        .iter()
        .filter_map(|path| classify_single_path_event(path, &event.kind, root, wiki_dir))
        .collect()
}

/// Verifica se um caminho bruto pertence a um diretório, comparando
/// componentes de path sem canonicalização.
///
/// Usa `Path::starts_with` que faz comparação sintática de componentes,
/// funcionando mesmo quando o arquivo não existe (ex: eventos `Remove`).
fn path_is_in(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// Verifica se dois caminhos brutos são iguais (comparação sintática).
fn path_equals(a: &Path, b: &Path) -> bool {
    a == b
}

/// Extrai o source_id de um caminho dentro de `sources/` ou `metadata/`.
///
/// Para `metadata/`, remove também a extensão `.json`.
///
/// Exemplos:
/// - `sources/abc123` → `Some("abc123")`
/// - `metadata/abc123.json` → `Some("abc123")`
fn extract_source_id(path: &Path, sources_dir: &Path, metadata_dir: &Path) -> Option<String> {
    let (relative, is_metadata) = if let Ok(rel) = path.strip_prefix(sources_dir) {
        (rel, false)
    } else if let Ok(rel) = path.strip_prefix(metadata_dir) {
        (rel, true)
    } else {
        return None;
    };

    let file_name = relative.file_name()?.to_str()?;

    // Ignora arquivos ocultos
    if file_name.is_empty() || file_name.starts_with('.') {
        return None;
    }

    let id = if is_metadata {
        file_name.strip_suffix(".json")?
    } else {
        file_name
    };

    if id.is_empty() {
        return None;
    }

    Some(id.to_string())
}

/// Extrai o slug de uma página a partir do caminho canônico.
///
/// Exemplo: `/root/.advwiki/pages/getting-started.md` → `Some("getting-started")`
fn extract_slug(canonical_path: &Path, pages_dir: &Path) -> Option<String> {
    let relative = canonical_path.strip_prefix(pages_dir).ok()?;
    let file_name = relative.file_name()?.to_str()?;

    // Apenas arquivos .md são páginas
    let slug = file_name.strip_suffix(".md")?;

    // Ignora arquivos ocultos (ex: .swp do vim)
    if slug.is_empty() || slug.starts_with('.') {
        return None;
    }

    // Garante que é um arquivo direto, não um subdiretório
    if relative.parent().map_or(false, |p| p != Path::new("")) {
        return None;
    }

    Some(slug.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};



    #[test]
    fn test_extract_slug_simple() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/pages/home.md");
        assert_eq!(extract_slug(&path, &pages), Some("home".into()));
    }

    #[test]
    fn test_extract_slug_with_hyphen() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/pages/getting-started.md");
        assert_eq!(
            extract_slug(&path, &pages),
            Some("getting-started".into())
        );
    }

    #[test]
    fn test_extract_slug_not_md() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/pages/config.json");
        assert_eq!(extract_slug(&path, &pages), None);
    }

    #[test]
    fn test_extract_slug_hidden_file() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/pages/.swp");
        assert_eq!(extract_slug(&path, &pages), None);
    }

    #[test]
    fn test_extract_slug_subdirectory() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/pages/sub/home.md");
        assert_eq!(extract_slug(&path, &pages), None);
    }

    #[test]
    fn test_extract_slug_outside_pages() {
        let pages = PathBuf::from("/root/.advwiki/pages");
        let path = PathBuf::from("/root/.advwiki/sources/abc123");
        assert_eq!(extract_slug(&path, &pages), None);
    }

    // ── Testes de WikiEvent ───────────────────────────────────────────────

    #[test]
    fn test_wiki_event_debug() {
        let event = WikiEvent::PageCreated {
            slug: "home".into(),
        };
        assert_eq!(
            format!("{:?}", event),
            "PageCreated { slug: \"home\" }"
        );
    }

    #[test]
    fn test_wiki_event_equality() {
        let a = WikiEvent::PageUpdated {
            slug: "api".into(),
        };
        let b = WikiEvent::PageUpdated {
            slug: "api".into(),
        };
        let c = WikiEvent::PageDeleted {
            slug: "api".into(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_wiki_event_clone() {
        let event = WikiEvent::IndexChanged;
        let cloned = event.clone();
        assert_eq!(event, cloned);
    }

    #[test]
    fn test_wiki_event_raw_source_equality() {
        let a = WikiEvent::RawSourceUpdated {
            source_id: "abc".into(),
        };
        let b = WikiEvent::RawSourceUpdated {
            source_id: "abc".into(),
        };
        assert_eq!(a, b);
        let c = WikiEvent::RawSourceDeleted {
            source_id: "abc".into(),
        };
        assert_ne!(a, c);
    }

    #[test]
    fn test_path_is_in_true() {
        let parent = PathBuf::from("/root/.advwiki/pages");
        let child = PathBuf::from("/root/.advwiki/pages/home.md");
        assert!(path_is_in(&child, &parent));
    }

    #[test]
    fn test_path_is_in_false() {
        let parent = PathBuf::from("/root/.advwiki/pages");
        let child = PathBuf::from("/root/.advwiki/sources/abc");
        assert!(!path_is_in(&child, &parent));
    }

    #[test]
    fn test_path_equals_true() {
        let a = PathBuf::from("/root/.advwikilog.md");
        let b = PathBuf::from("/root/.advwikilog.md");
        assert!(path_equals(&a, &b));
    }

    #[test]
    fn test_path_equals_false() {
        let a = PathBuf::from("/root/.advwikilog.md");
        let b = PathBuf::from("/root/rawindex.md");
        assert!(!path_equals(&a, &b));
    }

    #[test]
    fn test_extract_source_id_from_sources() {
        let sources = PathBuf::from("/root/.advwiki/sources");
        let metadata = PathBuf::from("/root/.advwiki/metadata");
        let path = PathBuf::from("/root/.advwiki/sources/abc123");
        assert_eq!(
            extract_source_id(&path, &sources, &metadata),
            Some("abc123".into())
        );
    }

    #[test]
    fn test_extract_source_id_from_metadata() {
        let sources = PathBuf::from("/root/.advwiki/sources");
        let metadata = PathBuf::from("/root/.advwiki/metadata");
        let path = PathBuf::from("/root/.advwiki/metadata/abc123.json");
        assert_eq!(
            extract_source_id(&path, &sources, &metadata),
            Some("abc123".into())
        );
    }

    #[test]
    fn test_extract_source_id_hidden_file() {
        let sources = PathBuf::from("/root/.advwiki/sources");
        let metadata = PathBuf::from("/root/.advwiki/metadata");
        let path = PathBuf::from("/root/.advwiki/sources/.hidden");
        assert_eq!(extract_source_id(&path, &sources, &metadata), None);
    }

    #[test]
    fn test_extract_source_id_not_in_dirs() {
        let sources = PathBuf::from("/root/.advwiki/sources");
        let metadata = PathBuf::from("/root/.advwiki/metadata");
        let path = PathBuf::from("/root/.advwiki/pages/home.md");
        assert_eq!(extract_source_id(&path, &sources, &metadata), None);
    }

    #[test]
    fn test_extract_source_id_metadata_without_json_ext() {
        let sources = PathBuf::from("/root/.advwiki/sources");
        let metadata = PathBuf::from("/root/.advwiki/metadata");
        let path = PathBuf::from("/root/.advwiki/metadata/abc123");
        // Em metadata/, esperamos .json; sem extensão não extrai
        assert_eq!(extract_source_id(&path, &sources, &metadata), None);
    }

    #[test]
    fn test_translate_event_page_rename_emits_delete_then_create() {
        let root = PathBuf::from("/root");
        let wiki_dir = root.join(".advwiki");
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![
                wiki_dir.join("pages/old-page.md"),
                wiki_dir.join("pages/new-page.md"),
            ],
            attrs: Default::default(),
        };

        let translated = translate_event(&event, &root, &wiki_dir);

        assert_eq!(
            translated,
            vec![
                WikiEvent::PageDeleted {
                    slug: "old-page".into(),
                },
                WikiEvent::PageCreated {
                    slug: "new-page".into(),
                },
            ]
        );
    }

    #[test]
    fn test_translate_event_raw_rename_emits_delete_then_create() {
        let root = PathBuf::from("/root");
        let wiki_dir = root.join(".advwiki");
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![
                wiki_dir.join("sources/old-id"),
                wiki_dir.join("sources/new-id"),
            ],
            attrs: Default::default(),
        };

        let translated = translate_event(&event, &root, &wiki_dir);

        assert_eq!(
            translated,
            vec![
                WikiEvent::RawSourceDeleted {
                    source_id: "old-id".into(),
                },
                WikiEvent::RawSourceUpdated {
                    source_id: "new-id".into(),
                },
            ]
        );
    }

    #[test]
    fn test_classify_single_path_rename_from_is_delete() {
        // Renomeação emitida como evento single-path `From` deve virar Delete,
        // não Update (senão a entrada antiga fica órfã no índice).
        let root = PathBuf::from("/root");
        let wiki_dir = root.join(".advwiki");

        let page = wiki_dir.join("pages/old-page.md");
        assert_eq!(
            classify_single_path_event(
                &page,
                &EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &root,
                &wiki_dir
            ),
            Some(WikiEvent::PageDeleted { slug: "old-page".into() })
        );

        let raw = wiki_dir.join("sources/old-id");
        assert_eq!(
            classify_single_path_event(
                &raw,
                &EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &root,
                &wiki_dir
            ),
            Some(WikiEvent::RawSourceDeleted { source_id: "old-id".into() })
        );
    }

    #[test]
    fn test_classify_single_path_rename_to_is_create() {
        let root = PathBuf::from("/root");
        let wiki_dir = root.join(".advwiki");

        let page = wiki_dir.join("pages/new-page.md");
        assert_eq!(
            classify_single_path_event(
                &page,
                &EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                &root,
                &wiki_dir
            ),
            Some(WikiEvent::PageCreated { slug: "new-page".into() })
        );
    }

    #[test]
    fn test_classify_single_path_event_preserves_regular_create_remove_behavior() {
        let root = PathBuf::from("/root");
        let wiki_dir = root.join(".advwiki");
        let page_path = wiki_dir.join("pages/home.md");
        let raw_path = wiki_dir.join("sources/abc123");

        assert_eq!(
            classify_single_path_event(&page_path, &EventKind::Create(CreateKind::Any), &root, &wiki_dir),
            Some(WikiEvent::PageCreated {
                slug: "home".into(),
            })
        );
        assert_eq!(
            classify_single_path_event(&raw_path, &EventKind::Remove(RemoveKind::Any), &root, &wiki_dir),
            Some(WikiEvent::RawSourceDeleted {
                source_id: "abc123".into(),
            })
        );
    }
}
