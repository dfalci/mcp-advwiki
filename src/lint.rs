// ── Módulo de Lint da Wiki ───────────────────────────────────────────────────
//
// Executa checks estruturais e de qualidade sobre o conteúdo da Wiki.
// Exposto via tool `lint_wiki` no servidor MCP.
//
// Checks por scope:
//
//   quick:
//     - links quebrados (wiki://page/slug apontando para página inexistente)
//     - páginas órfãs (nenhuma outra página as referencia)
//     - raw sources sem página derivada (source_id não citado em nenhuma página)
//     - páginas grandes (acima de LARGE_PAGE_THRESHOLD_BYTES)
//     - páginas sem seção "Veja também" / "See also"
//
//   all  (inclui quick +):
//     - páginas desatualizadas (mtime do arquivo > STALE_DAYS_THRESHOLD dias)
//     - decisões sem rationale (slug com padrão de decisão + sem seção de justificativa)
//     - páginas similares (Jaccard de tokens > SIMILARITY_THRESHOLD — candidatas a duplicata)

use crate::search::WikiSearchEngine;
use crate::storage::WikiFileManager;
use std::collections::{HashMap, HashSet};

const LARGE_PAGE_THRESHOLD_BYTES: usize = 50_000;
const STALE_DAYS_THRESHOLD: i64 = 90;
const SIMILARITY_THRESHOLD: f32 = 0.6;
const MIN_TOKENS_FOR_SIMILARITY: usize = 20;
const MIN_TOKEN_LEN: usize = 3;

const DECISION_SLUG_PATTERNS: &[&str] = &["decisao", "decision", "adr"];
const RATIONALE_HEADERS: &[&str] = &[
    "## Rationale",
    "## rationale",
    "## Justificativa",
    "## Motivação",
    "## Motivacao",
    "## Por que",
    "## Por quê",
];

// ── Estruturas de resultado ──────────────────────────────────────────────────

pub struct BrokenLink {
    pub source_slug: String,
    pub target_slug: String,
}

pub struct LargePage {
    pub slug: String,
    pub size_bytes: usize,
}

pub struct SimilarPagePair {
    pub slug_a: String,
    pub slug_b: String,
    pub similarity: f32,
}

pub struct LintReport {
    pub scope: String,
    // resumo
    pub page_count: usize,
    pub source_count: usize,
    pub index_doc_count: u64,
    pub index_consistent: bool,
    // erros estruturais
    pub broken_links: Vec<BrokenLink>,
    pub orphan_pages: Vec<String>,
    pub raw_without_pages: Vec<String>,
    // avisos de qualidade
    pub large_pages: Vec<LargePage>,
    pub missing_see_also: Vec<String>,
    // scope "all" apenas
    pub stale_pages: Vec<String>,
    pub decisions_without_rationale: Vec<String>,
    pub similar_pages: Vec<SimilarPagePair>,
}

impl LintReport {
    pub fn format_markdown(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!(
            "# Relatório de Lint da Wiki (scope: {})\n",
            self.scope
        ));

        // resumo
        lines.push("## Resumo\n".into());
        lines.push(format!("- Páginas: {}", self.page_count));
        lines.push(format!("- Raw sources: {}", self.source_count));
        lines.push(format!("- Documentos no índice: {}", self.index_doc_count));
        if self.index_consistent {
            lines.push("- Índice: ok (consistente com o disco)".into());
        } else {
            let expected = (self.page_count + self.source_count) as u64;
            lines.push(format!(
                "- Índice: warn ({} documentos no disco vs {} no índice)",
                expected, self.index_doc_count
            ));
        }
        lines.push("".into());

        format_section(
            &mut lines,
            "## Links Quebrados",
            &self.broken_links,
            |bl| {
                format!(
                    "- `{}` aponta para `{}`, mas a página não existe.",
                    bl.source_slug, bl.target_slug
                )
            },
            "_Nenhum link quebrado encontrado._",
        );

        format_section(
            &mut lines,
            "## Páginas Órfãs",
            &self.orphan_pages,
            |slug| format!("- `{slug}`"),
            "_Nenhuma página órfã encontrada._",
        );

        format_section(
            &mut lines,
            "## Raw Sources sem Página Derivada",
            &self.raw_without_pages,
            |id| format!("- `{id}`"),
            "_Todas as raw sources têm pelo menos uma página referenciando-as._",
        );

        format_section(
            &mut lines,
            &format!(
                "## Páginas Grandes (> {} KB)",
                LARGE_PAGE_THRESHOLD_BYTES / 1024
            ),
            &self.large_pages,
            |lp| format!("- `{}` — {} KB", lp.slug, lp.size_bytes / 1024),
            "_Nenhuma página acima do limite de tamanho._",
        );

        format_section(
            &mut lines,
            "## Páginas sem \"Veja também\"",
            &self.missing_see_also,
            |slug| format!("- `{slug}`"),
            "_Todas as páginas têm seção \"Veja também\"._",
        );

        if self.scope == "all" {
            format_section(
                &mut lines,
                &format!(
                    "## Páginas Desatualizadas (sem modificação há mais de {} dias)",
                    STALE_DAYS_THRESHOLD
                ),
                &self.stale_pages,
                |slug| format!("- `{slug}`"),
                "_Nenhuma página desatualizada encontrada._",
            );

            format_section(
                &mut lines,
                "## Decisões sem Rationale",
                &self.decisions_without_rationale,
                |slug| format!("- `{slug}`"),
                "_Todas as decisões têm seção de justificativa._",
            );

            format_section(
                &mut lines,
                &format!(
                    "## Páginas Similares (similaridade > {:.0}% — candidatas a duplicata ou fusão)",
                    SIMILARITY_THRESHOLD * 100.0
                ),
                &self.similar_pages,
                |p| {
                    format!(
                        "- `{}` e `{}` — {:.0}% de similaridade de conteúdo",
                        p.slug_a,
                        p.slug_b,
                        p.similarity * 100.0
                    )
                },
                "_Nenhum par de páginas com conteúdo excessivamente similar._",
            );
        }

        lines.join("\n")
    }
}

fn format_section<T>(
    lines: &mut Vec<String>,
    header: &str,
    items: &[T],
    format_item: impl Fn(&T) -> String,
    empty_msg: &str,
) {
    lines.push(format!("{header}\n"));
    if items.is_empty() {
        lines.push(empty_msg.into());
    } else {
        for item in items {
            lines.push(format_item(item));
        }
    }
    lines.push("".into());
}

// ── Execução principal ───────────────────────────────────────────────────────

pub async fn run_lint(
    scope: &str,
    file_manager: &WikiFileManager,
    search_engine: &WikiSearchEngine,
) -> anyhow::Result<LintReport> {
    // carrega todas as páginas e conteúdos
    let slugs = file_manager.list_pages().await?;
    let page_count = slugs.len();

    let mut page_contents: HashMap<String, String> = HashMap::new();
    for slug in &slugs {
        match file_manager.read_page(slug).await {
            Ok(content) => {
                page_contents.insert(slug.clone(), content);
            }
            Err(e) => {
                tracing::warn!(%slug, error = %e, "lint: falha ao ler página");
            }
        }
    }

    let page_set: HashSet<String> = slugs.into_iter().collect();

    // carrega raw sources
    let source_ids = file_manager.list_raw_sources().await?;
    let source_count = source_ids.len();

    // consistência do índice
    let index_doc_count = search_engine.doc_count()?;
    let index_consistent = (page_count + source_count) as u64 == index_doc_count;

    // grafo de links: monta mapa de links de saída e contagem de links de entrada
    let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
    let mut incoming: HashMap<String, usize> = HashMap::new();
    for slug in &page_set {
        incoming.entry(slug.clone()).or_insert(0);
    }
    for (slug, content) in &page_contents {
        let links = extract_wiki_page_links(content);
        for target in &links {
            *incoming.entry(target.clone()).or_insert(0) += 1;
        }
        outgoing.insert(slug.clone(), links);
    }

    // links quebrados
    let mut broken_links: Vec<BrokenLink> = outgoing
        .iter()
        .flat_map(|(slug, targets)| {
            targets
                .iter()
                .filter(|target| !page_set.contains(*target))
                .map(|target| BrokenLink {
                    source_slug: slug.clone(),
                    target_slug: target.clone(),
                })
        })
        .collect();
    broken_links.sort_by(|a, b| {
        a.source_slug
            .cmp(&b.source_slug)
            .then(a.target_slug.cmp(&b.target_slug))
    });

    // páginas órfãs
    let mut orphan_pages: Vec<String> = incoming
        .iter()
        .filter(|&(_, count)| *count == 0)
        .map(|(slug, _)| slug.clone())
        .collect();
    orphan_pages.sort();

    // raw sources sem página derivada
    let all_page_content: String = page_contents.values().cloned().collect::<Vec<_>>().join("\n");
    let mut raw_without_pages: Vec<String> = source_ids
        .iter()
        .filter(|id| !all_page_content.contains(id.as_str()))
        .cloned()
        .collect();
    raw_without_pages.sort();

    // páginas grandes
    let mut large_pages: Vec<LargePage> = page_contents
        .iter()
        .filter(|(_, content)| content.len() > LARGE_PAGE_THRESHOLD_BYTES)
        .map(|(slug, content)| LargePage {
            slug: slug.clone(),
            size_bytes: content.len(),
        })
        .collect();
    large_pages.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    // páginas sem "Veja também"
    let mut missing_see_also: Vec<String> = page_contents
        .iter()
        .filter(|(_, content)| {
            !content.contains("## Veja também")
                && !content.contains("## Veja Também")
                && !content.contains("## See also")
                && !content.contains("## See Also")
        })
        .map(|(slug, _)| slug.clone())
        .collect();
    missing_see_also.sort();

    // checks do scope "all"
    let (stale_pages, decisions_without_rationale, similar_pages) = if scope == "all" {
        let stale = check_stale_pages(file_manager, &page_contents).await;
        let decisions = check_decisions_without_rationale(&page_contents);
        let similar = check_similar_pages(&page_contents);
        (stale, decisions, similar)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    Ok(LintReport {
        scope: scope.to_string(),
        page_count,
        source_count,
        index_doc_count,
        index_consistent,
        broken_links,
        orphan_pages,
        raw_without_pages,
        large_pages,
        missing_see_also,
        stale_pages,
        decisions_without_rationale,
        similar_pages,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// extrai slugs referenciados via `wiki://page/{slug}` no conteúdo de uma página.
pub fn extract_wiki_page_links(content: &str) -> Vec<String> {
    let prefix = "wiki://page/";
    let mut links = Vec::new();
    let mut rest = content;

    while let Some(pos) = rest.find(prefix) {
        rest = &rest[pos + prefix.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
            .unwrap_or(rest.len());
        // slug não pode terminar com '.' (validate_slug rejeita) — strip de pontuação final
        let slug = rest[..end].trim_end_matches('.');
        if !slug.is_empty() {
            links.push(slug.to_string());
        }
        if end >= rest.len() {
            break;
        }
        rest = &rest[end..];
    }

    links.sort();
    links.dedup();
    links
}

async fn check_stale_pages(
    file_manager: &WikiFileManager,
    page_contents: &HashMap<String, String>,
) -> Vec<String> {
    let threshold = chrono::Utc::now() - chrono::TimeDelta::days(STALE_DAYS_THRESHOLD);
    let mut stale = Vec::new();

    for slug in page_contents.keys() {
        let path = file_manager
            .wiki_dir()
            .join("pages")
            .join(format!("{slug}.md"));

        if let Ok(meta) = tokio::fs::metadata(&path).await {
            if let Ok(modified) = meta.modified() {
                let modified_dt: chrono::DateTime<chrono::Utc> = modified.into();
                if modified_dt < threshold {
                    stale.push(slug.clone());
                }
            }
        }
    }

    stale.sort();
    stale
}

/// sinaliza páginas cujo slug indica uma decisão arquitetural mas que não têm
/// seção de justificativa. Heurística: slug contém `decisao`, `decision` ou `adr`.
fn check_decisions_without_rationale(page_contents: &HashMap<String, String>) -> Vec<String> {
    let mut flagged: Vec<String> = page_contents
        .iter()
        .filter(|(slug, content)| {
            is_decision_slug(slug) && !has_rationale_section(content)
        })
        .map(|(slug, _)| slug.clone())
        .collect();
    flagged.sort();
    flagged
}

fn is_decision_slug(slug: &str) -> bool {
    let lower = slug.to_lowercase();
    DECISION_SLUG_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

fn has_rationale_section(content: &str) -> bool {
    RATIONALE_HEADERS.iter().any(|h| content.contains(h))
}

/// detecta pares de páginas com conteúdo excessivamente similar (Jaccard > SIMILARITY_THRESHOLD).
/// páginas com poucos tokens únicos são ignoradas para evitar falsos positivos.
fn check_similar_pages(page_contents: &HashMap<String, String>) -> Vec<SimilarPagePair> {
    // tokeniza cada página uma única vez
    let token_sets: Vec<(String, HashSet<String>)> = page_contents
        .iter()
        .map(|(slug, content)| (slug.clone(), tokenize(content)))
        .filter(|(_, tokens)| tokens.len() >= MIN_TOKENS_FOR_SIMILARITY)
        .collect();

    let mut pairs: Vec<SimilarPagePair> = Vec::new();

    for i in 0..token_sets.len() {
        for j in (i + 1)..token_sets.len() {
            let (slug_a, tokens_a) = &token_sets[i];
            let (slug_b, tokens_b) = &token_sets[j];

            let similarity = jaccard(tokens_a, tokens_b);
            if similarity >= SIMILARITY_THRESHOLD {
                // garante ordem lexicográfica estável no par
                let (a, b) = if slug_a <= slug_b {
                    (slug_a.clone(), slug_b.clone())
                } else {
                    (slug_b.clone(), slug_a.clone())
                };
                pairs.push(SimilarPagePair {
                    slug_a: a,
                    slug_b: b,
                    similarity,
                });
            }
        }
    }

    pairs.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.slug_a.cmp(&b.slug_a))
    });
    pairs
}

fn tokenize(content: &str) -> HashSet<String> {
    content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= MIN_TOKEN_LEN)
        .map(|w| w.to_lowercase())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::WikiSearchEngine;
    use crate::storage::WikiFileManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_engine(root: &std::path::Path) -> WikiSearchEngine {
        WikiSearchEngine::new(root.join(".advwiki/index")).unwrap()
    }

    async fn make_manager(root: std::path::PathBuf) -> Arc<WikiFileManager> {
        let m = Arc::new(WikiFileManager::new(Some(root)));
        m.init().await.unwrap();
        m
    }

    // ── extract_wiki_page_links ──────────────────────────────────────────────

    #[test]
    fn test_extract_links_single() {
        let content = "veja [esta página](wiki://page/getting-started) para mais";
        assert_eq!(extract_wiki_page_links(content), vec!["getting-started"]);
    }

    #[test]
    fn test_extract_links_multiple() {
        let content = "links: wiki://page/home e wiki://page/api-reference aqui";
        assert_eq!(
            extract_wiki_page_links(content),
            vec!["api-reference", "home"]
        );
    }

    #[test]
    fn test_extract_links_dedup() {
        let content = "wiki://page/home aparece wiki://page/home novamente";
        assert_eq!(extract_wiki_page_links(content), vec!["home"]);
    }

    #[test]
    fn test_extract_links_empty() {
        assert!(extract_wiki_page_links("sem links aqui").is_empty());
    }

    #[test]
    fn test_extract_links_no_false_positives() {
        let content = "wiki://log e wiki://index e raw://source/abc não são page links";
        assert!(extract_wiki_page_links(content).is_empty());
    }

    #[test]
    fn test_extract_links_at_end_of_string() {
        let content = "veja wiki://page/home";
        assert_eq!(extract_wiki_page_links(content), vec!["home"]);
    }

    #[test]
    fn test_extract_links_with_trailing_punctuation() {
        let content = "confira wiki://page/home.";
        assert_eq!(extract_wiki_page_links(content), vec!["home"]);
    }

    // ── is_decision_slug / has_rationale_section ─────────────────────────────

    #[test]
    fn test_is_decision_slug_matches_decisao() {
        assert!(is_decision_slug("decisao-cache-de-sessao"));
        assert!(is_decision_slug("decisao-uso-de-tantivy"));
    }

    #[test]
    fn test_is_decision_slug_matches_decision() {
        assert!(is_decision_slug("decision-use-postgres"));
    }

    #[test]
    fn test_is_decision_slug_matches_adr() {
        assert!(is_decision_slug("adr-001-autenticacao"));
        assert!(is_decision_slug("adr-042"));
    }

    #[test]
    fn test_is_decision_slug_no_match() {
        assert!(!is_decision_slug("getting-started"));
        assert!(!is_decision_slug("arquitetura-geral"));
        assert!(!is_decision_slug("home"));
    }

    #[test]
    fn test_has_rationale_section_pt() {
        assert!(has_rationale_section("## Justificativa\ntexto"));
        assert!(has_rationale_section("## Motivação\ntexto"));
        assert!(has_rationale_section("## Por que\ntexto"));
    }

    #[test]
    fn test_has_rationale_section_en() {
        assert!(has_rationale_section("## Rationale\ntexto"));
        assert!(has_rationale_section("## rationale\ntexto"));
    }

    #[test]
    fn test_has_rationale_section_absent() {
        assert!(!has_rationale_section("## Decisão\nConteúdo sem justificativa"));
    }

    // ── tokenize / jaccard ───────────────────────────────────────────────────

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("hello world foo bar");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(!tokens.contains("fo")); // len < MIN_TOKEN_LEN
    }

    #[test]
    fn test_tokenize_lowercases() {
        let tokens = tokenize("Hello WORLD");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(!tokens.contains("Hello"));
    }

    #[test]
    fn test_tokenize_splits_on_punctuation() {
        let tokens = tokenize("foo,bar.baz:qux");
        assert!(tokens.contains("foo"));
        assert!(tokens.contains("bar"));
        assert!(tokens.contains("baz"));
        assert!(tokens.contains("qux"));
    }

    #[test]
    fn test_jaccard_identical() {
        let a: HashSet<String> = ["apple", "banana", "cherry"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b = a.clone();
        assert!((jaccard(&a, &b) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_jaccard_disjoint() {
        let a: HashSet<String> = ["apple", "banana"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["cherry", "date"].iter().map(|s| s.to_string()).collect();
        assert!((jaccard(&a, &b)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_jaccard_partial_overlap() {
        let a: HashSet<String> = ["apple", "banana", "cherry"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: HashSet<String> = ["apple", "banana", "date"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // intersection=2, union=4 → 0.5
        let j = jaccard(&a, &b);
        assert!((j - 0.5).abs() < 1e-5, "expected 0.5, got {j}");
    }

    #[test]
    fn test_jaccard_both_empty() {
        let a: HashSet<String> = HashSet::new();
        let b: HashSet<String> = HashSet::new();
        assert!((jaccard(&a, &b)).abs() < f32::EPSILON);
    }

    // ── check_decisions_without_rationale ────────────────────────────────────

    #[test]
    fn test_decision_without_rationale_flagged() {
        let mut pages = HashMap::new();
        pages.insert(
            "decisao-cache".to_string(),
            "## Decisão\nUsar Redis.".to_string(),
        );
        let flagged = check_decisions_without_rationale(&pages);
        assert_eq!(flagged, vec!["decisao-cache"]);
    }

    #[test]
    fn test_decision_with_rationale_ok() {
        let mut pages = HashMap::new();
        pages.insert(
            "decisao-cache".to_string(),
            "## Decisão\nUsar Redis.\n\n## Justificativa\nPerformance.".to_string(),
        );
        let flagged = check_decisions_without_rationale(&pages);
        assert!(flagged.is_empty());
    }

    #[test]
    fn test_non_decision_page_not_flagged() {
        let mut pages = HashMap::new();
        pages.insert(
            "getting-started".to_string(),
            "Guia inicial sem rationale.".to_string(),
        );
        let flagged = check_decisions_without_rationale(&pages);
        assert!(flagged.is_empty());
    }

    // ── check_similar_pages ──────────────────────────────────────────────────

    #[test]
    fn test_similar_pages_detects_near_duplicate() {
        // gera conteúdo com muitas palavras comuns — acima do threshold
        let words: Vec<String> = (0..50).map(|i| format!("palavra{i:03}")).collect();
        let base = words.join(" ");

        let mut pages = HashMap::new();
        pages.insert("page-a".to_string(), base.clone());
        pages.insert("page-b".to_string(), base.clone()); // idênticas → Jaccard = 1.0

        let pairs = check_similar_pages(&pages);
        assert_eq!(pairs.len(), 1);
        assert!((pairs[0].similarity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_similar_pages_ignores_short_pages() {
        let mut pages = HashMap::new();
        // menos de MIN_TOKENS_FOR_SIMILARITY tokens únicos — deve ser ignorada
        pages.insert("short-a".to_string(), "foo bar".to_string());
        pages.insert("short-b".to_string(), "foo bar".to_string());

        let pairs = check_similar_pages(&pages);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_similar_pages_distinct_pages_not_flagged() {
        let a: Vec<String> = (0..50).map(|i| format!("alfa{i:03}")).collect();
        let b: Vec<String> = (0..50).map(|i| format!("beta{i:03}")).collect();

        let mut pages = HashMap::new();
        pages.insert("page-a".to_string(), a.join(" "));
        pages.insert("page-b".to_string(), b.join(" "));

        let pairs = check_similar_pages(&pages);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_similar_pages_pair_order_is_lexicographic() {
        let words: Vec<String> = (0..50).map(|i| format!("termo{i:03}")).collect();
        let base = words.join(" ");

        let mut pages = HashMap::new();
        pages.insert("zzz-page".to_string(), base.clone());
        pages.insert("aaa-page".to_string(), base.clone());

        let pairs = check_similar_pages(&pages);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].slug_a, "aaa-page");
        assert_eq!(pairs[0].slug_b, "zzz-page");
    }

    // ── run_lint (integração) ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_lint_empty_wiki_has_no_issues() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());
        let report = run_lint("quick", &fm, &engine).await.unwrap();

        assert_eq!(report.page_count, 0);
        assert!(report.broken_links.is_empty());
        assert!(report.orphan_pages.is_empty());
        assert!(report.raw_without_pages.is_empty());
        assert!(report.large_pages.is_empty());
        assert!(report.missing_see_also.is_empty());
    }

    #[tokio::test]
    async fn test_lint_detects_broken_link() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("home", "veja wiki://page/inexistente").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert_eq!(report.broken_links.len(), 1);
        assert_eq!(report.broken_links[0].source_slug, "home");
        assert_eq!(report.broken_links[0].target_slug, "inexistente");
    }

    #[tokio::test]
    async fn test_lint_no_broken_links_when_target_exists() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("home", "veja wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(report.broken_links.is_empty());
    }

    #[tokio::test]
    async fn test_lint_detects_orphan_page() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("home", "conteúdo sem links").await.unwrap();
        fm.write_page("orphan", "sou um órfão").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(report.orphan_pages.contains(&"home".to_string()));
        assert!(report.orphan_pages.contains(&"orphan".to_string()));
    }

    #[tokio::test]
    async fn test_lint_linked_page_not_orphan() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("home", "veja wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(!report.orphan_pages.contains(&"about".to_string()));
        assert!(report.orphan_pages.contains(&"home".to_string()));
    }

    #[tokio::test]
    async fn test_lint_detects_raw_source_without_page() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        let meta = crate::storage::RawSourceMetadata::new("abc123".into(), 10);
        fm.write_raw_source("abc123", &meta, "conteúdo bruto").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(report.raw_without_pages.contains(&"abc123".to_string()));
    }

    #[tokio::test]
    async fn test_lint_raw_source_referenced_in_page_is_ok() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        let meta = crate::storage::RawSourceMetadata::new("abc123".into(), 10);
        fm.write_raw_source("abc123", &meta, "conteúdo bruto").await.unwrap();
        fm.write_page("home", "baseado na source abc123").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(!report.raw_without_pages.contains(&"abc123".to_string()));
    }

    #[tokio::test]
    async fn test_lint_detects_large_page() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        let large_content = "A".repeat(LARGE_PAGE_THRESHOLD_BYTES + 1);
        fm.write_page("huge", &large_content).await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert_eq!(report.large_pages.len(), 1);
        assert_eq!(report.large_pages[0].slug, "huge");
    }

    #[tokio::test]
    async fn test_lint_detects_missing_see_also() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("no-see-also", "conteúdo sem seção de veja também").await.unwrap();
        fm.write_page(
            "has-see-also",
            "conteúdo\n\n## Veja também\n- wiki://page/no-see-also",
        )
        .await
        .unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(report.missing_see_also.contains(&"no-see-also".to_string()));
        assert!(!report.missing_see_also.contains(&"has-see-also".to_string()));
    }

    #[tokio::test]
    async fn test_lint_see_also_accepts_english_variant() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("english", "content\n\n## See also\n- link").await.unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(!report.missing_see_also.contains(&"english".to_string()));
    }

    #[tokio::test]
    async fn test_lint_index_consistency_detected() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("page-a", "conteúdo").await.unwrap();
        let report = run_lint("quick", &fm, &engine).await.unwrap();
        assert!(!report.index_consistent);
    }

    #[tokio::test]
    async fn test_lint_all_scope_detects_decision_without_rationale() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("decisao-usar-redis", "## Decisão\nUsar Redis.")
            .await
            .unwrap();

        let report = run_lint("all", &fm, &engine).await.unwrap();
        assert!(report
            .decisions_without_rationale
            .contains(&"decisao-usar-redis".to_string()));
    }

    #[tokio::test]
    async fn test_lint_all_scope_ok_decision_with_rationale() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page(
            "decisao-usar-redis",
            "## Decisão\nUsar Redis.\n\n## Justificativa\nPerformance.",
        )
        .await
        .unwrap();

        let report = run_lint("all", &fm, &engine).await.unwrap();
        assert!(report.decisions_without_rationale.is_empty());
    }

    #[tokio::test]
    async fn test_lint_all_scope_detects_similar_pages() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        // gera conteúdo com MIN_TOKENS_FOR_SIMILARITY+ tokens únicos idênticos
        let words: Vec<String> = (0..60).map(|i| format!("palavra{i:03}")).collect();
        let content = words.join(" ");

        fm.write_page("page-original", &content).await.unwrap();
        fm.write_page("page-duplicada", &content).await.unwrap();

        let report = run_lint("all", &fm, &engine).await.unwrap();
        assert_eq!(report.similar_pages.len(), 1);
        assert!((report.similar_pages[0].similarity - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_lint_quick_scope_skips_all_checks() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        fm.write_page("decisao-sem-rationale", "## Decisão\nSem justificativa.")
            .await
            .unwrap();

        let report = run_lint("quick", &fm, &engine).await.unwrap();
        // scope quick não executa checks de "all"
        assert!(report.decisions_without_rationale.is_empty());
        assert!(report.similar_pages.is_empty());
        assert!(report.stale_pages.is_empty());
    }

    #[tokio::test]
    async fn test_lint_format_markdown_has_all_sections() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let engine = make_engine(dir.path());

        let report = run_lint("all", &fm, &engine).await.unwrap();
        let md = report.format_markdown();

        assert!(md.contains("# Relatório de Lint da Wiki"));
        assert!(md.contains("## Resumo"));
        assert!(md.contains("## Links Quebrados"));
        assert!(md.contains("## Páginas Órfãs"));
        assert!(md.contains("## Raw Sources sem Página Derivada"));
        assert!(md.contains("## Páginas Grandes"));
        assert!(md.contains("## Páginas sem \"Veja também\""));
        assert!(md.contains("## Páginas Desatualizadas"));
        assert!(md.contains("## Decisões sem Rationale"));
        assert!(md.contains("## Páginas Similares"));
    }
}
