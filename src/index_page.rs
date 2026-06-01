// ── Módulo da Página de Índice Navegável ─────────────────────────────────────
//
// Gera a página `wiki://page/index`: um índice navegável (markdown) que agrupa
// todas as páginas por `type` e `project` do frontmatter, com wikilinks. É o
// ponto de entrada do vault no Obsidian.
//
// Diferente do índice de busca (Tantivy, reativo via watcher) e do `rawindex.md`
// (upsert automático em cada raw source), esta página é um artefato derivado.
// É regenerada:
//   - uma vez no boot da instância primary;
//   - automaticamente (com debounce) sempre que páginas mudam.
//
// ## Anti-loop
//
// Regenerar a página escreve `wiki://page/index`, o que o watcher reporta como
// um `PageUpdated { slug: "index" }`. Se esse evento agendasse outra
// regeneração, teríamos um loop infinito (regenera → evento → regenera → ...).
// `event_dirties_index` retorna `false` para o slug `index` justamente para
// quebrar esse ciclo. É uma função pura — testada sem subir o watcher.

use crate::storage::WikiFileManager;
use crate::watcher::WikiEvent;
use std::collections::BTreeMap;

/// Slug da página de índice gerada.
pub const INDEX_SLUG: &str = "index";

/// `true` se um evento de watcher exige regenerar a página de índice.
///
/// Mudanças na própria página `index` retornam `false` — é isto que evita o
/// loop regenera→evento→regenera. Raw sources, log e rawindex não compõem o
/// índice de páginas, então também não a sujam.
pub fn event_dirties_index(event: &WikiEvent) -> bool {
    match event {
        WikiEvent::PageCreated { slug }
        | WikiEvent::PageUpdated { slug }
        | WikiEvent::PageDeleted { slug } => slug != INDEX_SLUG,
        _ => false,
    }
}

/// (Re)gera a página `wiki://page/index` a partir do estado atual no disco e a
/// grava. Retorna o número de páginas indexadas (exclui a própria `index`).
///
/// Idempotente dentro do mesmo dia: regenerar sem mudanças produz conteúdo
/// idêntico (a única parte data-dependente é `updated_at`, fixada em "hoje").
pub async fn generate(file_manager: &WikiFileManager) -> anyhow::Result<usize> {
    struct PageMeta {
        slug: String,
        page_type: Option<String>,
        project: Option<String>,
        status: Option<String>,
    }

    let slugs = file_manager.list_pages().await?;

    let mut pages: Vec<PageMeta> = Vec::new();
    for slug in &slugs {
        if slug == INDEX_SLUG {
            continue;
        }
        let fm = match file_manager.read_page(slug).await {
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

    // Agrupa por tipo (BTreeMap mantém ordem alfabética).
    let mut by_type: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut untyped: Vec<usize> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        match &page.page_type {
            Some(t) => by_type.entry(t.clone()).or_default().push(i),
            None => untyped.push(i),
        }
    }

    // Agrupa por projeto.
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
        format!("> Gerado automaticamente pelo AdvWiki em {today}. Não edite manualmente."),
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
                if let Some(p) = &page.project {
                    meta.push(format!("project: {p}"));
                }
                if let Some(s) = &page.status {
                    meta.push(format!("status: {s}"));
                }
                let suffix = if meta.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", meta.join(", "))
                };
                lines.push(format!("- [[{}]]{suffix}", page.slug));
            }
            lines.push(String::new());
        }
        if !untyped.is_empty() {
            lines.push("### (sem tipo)".into());
            lines.push(String::new());
            for &i in &untyped {
                let page = &pages[i];
                let mut meta = Vec::new();
                if let Some(p) = &page.project {
                    meta.push(format!("project: {p}"));
                }
                if let Some(s) = &page.status {
                    meta.push(format!("status: {s}"));
                }
                let suffix = if meta.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", meta.join(", "))
                };
                lines.push(format!("- [[{}]]{suffix}", page.slug));
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
                if let Some(t) = &page.page_type {
                    meta.push(format!("type: {t}"));
                }
                if let Some(s) = &page.status {
                    meta.push(format!("status: {s}"));
                }
                let suffix = if meta.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", meta.join(", "))
                };
                lines.push(format!("- [[{}]]{suffix}", page.slug));
            }
            lines.push(String::new());
        }
    }

    lines.push("---".into());
    lines.push(format!("_Total: {} páginas indexadas._", pages.len()));

    let content = lines.join("\n");
    file_manager.write_page(INDEX_SLUG, &content).await?;
    tracing::debug!(pages = pages.len(), "Página de índice navegável (re)gerada");

    Ok(pages.len())
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn make_fm(dir: &TempDir) -> WikiFileManager {
        let fm = WikiFileManager::new(Some(dir.path().to_path_buf()));
        fm.init().await.unwrap();
        fm
    }

    // ── Anti-loop: a invariante que garante que não trava ────────────────────

    #[test]
    fn test_event_on_index_page_does_not_dirty() {
        // O cerne do anti-loop: a escrita da própria `index` gera este evento,
        // que NÃO pode reagendar regeneração.
        assert!(!event_dirties_index(&WikiEvent::PageUpdated {
            slug: INDEX_SLUG.to_string()
        }));
        assert!(!event_dirties_index(&WikiEvent::PageCreated {
            slug: INDEX_SLUG.to_string()
        }));
        assert!(!event_dirties_index(&WikiEvent::PageDeleted {
            slug: INDEX_SLUG.to_string()
        }));
    }

    #[test]
    fn test_event_on_real_page_dirties() {
        assert!(event_dirties_index(&WikiEvent::PageCreated { slug: "a".into() }));
        assert!(event_dirties_index(&WikiEvent::PageUpdated { slug: "a".into() }));
        assert!(event_dirties_index(&WikiEvent::PageDeleted { slug: "a".into() }));
    }

    #[test]
    fn test_non_page_events_do_not_dirty() {
        // Raw sources, log e rawindex não compõem o índice de páginas.
        assert!(!event_dirties_index(&WikiEvent::RawSourceUpdated {
            source_id: "x".into()
        }));
        assert!(!event_dirties_index(&WikiEvent::RawSourceDeleted {
            source_id: "x".into()
        }));
        assert!(!event_dirties_index(&WikiEvent::IndexChanged));
        assert!(!event_dirties_index(&WikiEvent::LogChanged));
        assert!(!event_dirties_index(&WikiEvent::Unknown("ruído".into())));
    }

    /// Prova o ciclo completo: regenerar a index escreve a página, e o evento
    /// que essa escrita gera no watcher não reagenda regeneração — o loop
    /// converge em um passo.
    #[tokio::test]
    async fn test_regenerating_does_not_request_another_regen() {
        let dir = TempDir::new().unwrap();
        let fm = make_fm(&dir).await;
        fm.write_page("a", "---\ntype: service\n---\n# A").await.unwrap();

        let n = generate(&fm).await.unwrap();
        assert_eq!(n, 1);

        // O watcher reportaria a escrita da index como este evento:
        let self_event = WikiEvent::PageUpdated {
            slug: INDEX_SLUG.to_string(),
        };
        assert!(
            !event_dirties_index(&self_event),
            "regenerar a index NÃO pode reagendar regeneração (anti-loop)"
        );
    }

    // ── generate ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_generate_groups_by_type_and_project() {
        let dir = TempDir::new().unwrap();
        let fm = make_fm(&dir).await;

        fm.write_page("auth-service", "---\ntype: service\nproject: auth\nstatus: active\n---\n# Auth")
            .await
            .unwrap();
        fm.write_page("auth-adr-001", "---\ntype: decision\nproject: auth\nstatus: accepted\n---\n# ADR 001")
            .await
            .unwrap();
        fm.write_page("no-frontmatter", "# Plain page without frontmatter")
            .await
            .unwrap();

        let count = generate(&fm).await.unwrap();
        assert_eq!(count, 3);

        let index = fm.read_page("index").await.unwrap();
        assert!(index.contains("### service"));
        assert!(index.contains("### decision"));
        assert!(index.contains("### (sem tipo)"));
        assert!(index.contains("## Por Projeto"));
        assert!(index.contains("### auth"));
        assert!(index.contains("[[auth-service]]"));
        assert!(index.contains("[[no-frontmatter]]"));
        // A página de índice não se lista (evita auto-referência).
        assert!(!index.contains("[[index]]"));
    }

    #[tokio::test]
    async fn test_generate_excludes_itself_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let fm = make_fm(&dir).await;
        fm.write_page("a", "---\ntype: service\n---\n# A").await.unwrap();
        fm.write_page("b", "---\ntype: note\n---\n# B").await.unwrap();

        // primeira geração
        let n1 = generate(&fm).await.unwrap();
        let first = fm.read_page("index").await.unwrap();

        // a `index` agora existe em disco; a próxima geração não a conta nem
        // produz conteúdo diferente (idempotente no mesmo dia).
        let n2 = generate(&fm).await.unwrap();
        let second = fm.read_page("index").await.unwrap();

        assert_eq!(n1, 2);
        assert_eq!(n2, 2, "a própria página index não pode ser contada");
        assert_eq!(first, second, "regenerar sem mudanças deve ser idempotente");
        // não criou nenhuma página espúria (ex.: 'index2')
        let pages = fm.list_pages().await.unwrap();
        assert_eq!(pages.iter().filter(|s| s.starts_with("index")).count(), 1);
    }

    #[tokio::test]
    async fn test_generate_empty_wiki() {
        let dir = TempDir::new().unwrap();
        let fm = make_fm(&dir).await;
        let n = generate(&fm).await.unwrap();
        assert_eq!(n, 0);
        let index = fm.read_page("index").await.unwrap();
        assert!(index.contains("_Nenhuma página encontrada._"));
    }
}
