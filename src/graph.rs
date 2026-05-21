// ── Módulo de Grafo de Links da Wiki ─────────────────────────────────────────
//
// Modela a Wiki como um grafo de conhecimento navegável.
//
// As arestas vêm de duas fontes:
//   - links `wiki://page/{slug}` no corpo das páginas;
//   - o campo `related` do frontmatter YAML.
//
// Quando um par de páginas é ligado pelas duas fontes, a aresta é classificada
// como `Body` (link explícito no corpo vence a declaração no frontmatter).
//
// Exposto via tools MCP: `wiki_graph`, `backlinks`, `orphans`, `related_pages`,
// `link_suggestions`.

use crate::storage::WikiFileManager;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Slug da página de índice gerada por `rebuild_wiki_index`. Ela linka todas as
/// páginas, então é ignorada como origem de links no cálculo de órfãos e
/// excluída do resultado de órfãos (é gerada, não é alvo de linkagem manual).
const GENERATED_INDEX_SLUG: &str = "index";

const SAME_PROJECT_BOOST: f32 = 0.15;
const SHARED_TAG_BOOST: f32 = 0.05;
const MAX_TAG_BOOST: f32 = 0.20;

// ── Tipos ────────────────────────────────────────────────────────────────────

/// Origem de uma aresta do grafo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Link `wiki://page/{slug}` no corpo da página.
    Body,
    /// Entrada no campo `related` do frontmatter.
    Related,
}

/// Rótulo legível da origem de uma aresta.
pub fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Body => "link no corpo",
        EdgeKind::Related => "frontmatter related",
    }
}

/// Grau de conectividade de uma página.
pub struct HubEntry {
    pub slug: String,
    pub in_degree: usize,
    pub out_degree: usize,
}

/// Natureza da relação entre uma página e um vizinho no grafo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// As duas páginas apontam uma para a outra.
    Bidirectional,
    /// A relação foi declarada no campo `related` do frontmatter.
    Declared,
    /// A página de origem aponta para o vizinho.
    LinksTo,
    /// O vizinho aponta para a página de origem.
    LinkedFrom,
}

impl Relation {
    /// Posição de ordenação (menor = relação mais forte).
    fn rank(self) -> u8 {
        match self {
            Relation::Bidirectional => 0,
            Relation::Declared => 1,
            Relation::LinksTo => 2,
            Relation::LinkedFrom => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Relation::Bidirectional => "bidirecional",
            Relation::Declared => "declarado em related",
            Relation::LinksTo => "aponta para",
            Relation::LinkedFrom => "apontado por",
        }
    }
}

/// Vizinho de uma página, com a natureza da relação.
pub struct RelatedEntry {
    pub slug: String,
    pub relation: Relation,
}

/// Sugestão de link entre duas páginas que ainda não estão conectadas.
pub struct LinkSuggestion {
    pub from: String,
    pub to: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

/// Grafo de links da Wiki. Cada nó é uma página existente; cada aresta é um
/// link direcionado de uma página para outra.
pub struct WikiGraph {
    /// Todas as páginas existentes (slugs).
    nodes: BTreeSet<String>,
    /// `slug` → arestas de saída `(alvo, origem)`.
    outgoing: HashMap<String, Vec<(String, EdgeKind)>>,
    /// `slug` → arestas de entrada `(origem, tipo)`.
    incoming: HashMap<String, Vec<(String, EdgeKind)>>,
}

// ── Construção ───────────────────────────────────────────────────────────────

impl WikiGraph {
    /// Constrói o grafo lendo todas as páginas da Wiki.
    pub async fn build(file_manager: &WikiFileManager) -> anyhow::Result<WikiGraph> {
        let slugs = file_manager.list_pages().await?;
        let nodes: BTreeSet<String> = slugs.iter().cloned().collect();

        // (origem, alvo) → tipo. `Body` sobrescreve `Related` no mesmo par.
        let mut edges: HashMap<(String, String), EdgeKind> = HashMap::new();

        for slug in &slugs {
            let content = match file_manager.read_page(slug).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(%slug, error = %e, "graph: falha ao ler página");
                    continue;
                }
            };

            // links wiki://page/ no corpo
            for target in extract_wiki_page_links(&content) {
                if target == *slug {
                    continue; // ignora self-link
                }
                edges.insert((slug.clone(), target), EdgeKind::Body);
            }

            // campo `related` do frontmatter
            if let Some(fm) = crate::frontmatter::parse_frontmatter(&content) {
                for target in fm.related {
                    if target == *slug {
                        continue;
                    }
                    edges
                        .entry((slug.clone(), target))
                        .or_insert(EdgeKind::Related);
                }
            }
        }

        let mut outgoing: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
        for slug in &slugs {
            outgoing.entry(slug.clone()).or_default();
            incoming.entry(slug.clone()).or_default();
        }
        for ((from, to), kind) in edges {
            outgoing
                .entry(from.clone())
                .or_default()
                .push((to.clone(), kind));
            incoming.entry(to).or_default().push((from, kind));
        }
        // ordena para saída determinística
        for v in outgoing.values_mut() {
            v.sort_by(|a, b| a.0.cmp(&b.0));
        }
        for v in incoming.values_mut() {
            v.sort_by(|a, b| a.0.cmp(&b.0));
        }

        Ok(WikiGraph {
            nodes,
            outgoing,
            incoming,
        })
    }

    // ── Consultas ────────────────────────────────────────────────────────────

    /// Número de páginas (nós).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Número de links (arestas).
    pub fn edge_count(&self) -> usize {
        self.outgoing.values().map(|v| v.len()).sum()
    }

    /// Verifica se `slug` é uma página existente.
    pub fn contains(&self, slug: &str) -> bool {
        self.nodes.contains(slug)
    }

    /// Páginas que apontam para `slug` (backlinks).
    pub fn backlinks(&self, slug: &str) -> Vec<(&str, EdgeKind)> {
        self.incoming
            .get(slug)
            .map(|v| v.iter().map(|(s, k)| (s.as_str(), *k)).collect())
            .unwrap_or_default()
    }

    /// Verifica se existe aresta entre `a` e `b` em qualquer direção.
    pub fn has_edge(&self, a: &str, b: &str) -> bool {
        let a_to_b = self
            .outgoing
            .get(a)
            .map(|v| v.iter().any(|(t, _)| t == b))
            .unwrap_or(false);
        let b_to_a = self
            .outgoing
            .get(b)
            .map(|v| v.iter().any(|(t, _)| t == a))
            .unwrap_or(false);
        a_to_b || b_to_a
    }

    /// Páginas sem nenhum backlink. Links vindos da página `index` gerada são
    /// ignorados e a própria `index` é excluída do resultado.
    pub fn orphans(&self) -> Vec<String> {
        let mut orphans: Vec<String> = self
            .nodes
            .iter()
            .filter(|slug| slug.as_str() != GENERATED_INDEX_SLUG)
            .filter(|slug| {
                self.incoming
                    .get(slug.as_str())
                    .map(|v| v.iter().all(|(from, _)| from == GENERATED_INDEX_SLUG))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        orphans.sort();
        orphans
    }

    /// Arestas cujo alvo não é uma página existente.
    pub fn broken_links(&self) -> Vec<(String, String)> {
        let mut broken: Vec<(String, String)> = Vec::new();
        for (from, targets) in &self.outgoing {
            for (to, _) in targets {
                if !self.nodes.contains(to) {
                    broken.push((from.clone(), to.clone()));
                }
            }
        }
        broken.sort();
        broken
    }

    /// Páginas ordenadas por grau total (entrada + saída) decrescente.
    pub fn hubs(&self) -> Vec<HubEntry> {
        let mut hubs: Vec<HubEntry> = self
            .nodes
            .iter()
            .map(|slug| HubEntry {
                slug: slug.clone(),
                in_degree: self.incoming.get(slug).map(|v| v.len()).unwrap_or(0),
                out_degree: self.outgoing.get(slug).map(|v| v.len()).unwrap_or(0),
            })
            .collect();
        hubs.sort_by(|a, b| {
            (b.in_degree + b.out_degree)
                .cmp(&(a.in_degree + a.out_degree))
                .then(a.slug.cmp(&b.slug))
        });
        hubs
    }

    /// Vizinhos de `slug` rankeados pela força da relação.
    pub fn related(&self, slug: &str) -> Vec<RelatedEntry> {
        let out: HashMap<&str, EdgeKind> = self
            .outgoing
            .get(slug)
            .map(|v| v.iter().map(|(s, k)| (s.as_str(), *k)).collect())
            .unwrap_or_default();
        let inc: HashMap<&str, EdgeKind> = self
            .incoming
            .get(slug)
            .map(|v| v.iter().map(|(s, k)| (s.as_str(), *k)).collect())
            .unwrap_or_default();

        let mut neighbors: BTreeSet<&str> = BTreeSet::new();
        neighbors.extend(out.keys().copied());
        neighbors.extend(inc.keys().copied());

        let mut entries: Vec<RelatedEntry> = neighbors
            .into_iter()
            .map(|q| {
                let o = out.get(q).copied();
                let i = inc.get(q).copied();
                let relation = if o.is_some() && i.is_some() {
                    Relation::Bidirectional
                } else if o == Some(EdgeKind::Related) || i == Some(EdgeKind::Related) {
                    Relation::Declared
                } else if o.is_some() {
                    Relation::LinksTo
                } else {
                    Relation::LinkedFrom
                };
                RelatedEntry {
                    slug: q.to_string(),
                    relation,
                }
            })
            .collect();

        entries.sort_by(|a, b| {
            a.relation
                .rank()
                .cmp(&b.relation.rank())
                .then(a.slug.cmp(&b.slug))
        });
        entries
    }

    // ── Renderização ─────────────────────────────────────────────────────────

    /// Renderiza o grafo em Markdown no formato pedido (`summary`, `full` ou
    /// `mermaid`). Qualquer outro valor cai em `summary`.
    pub fn render(&self, format: &str) -> String {
        match format {
            "mermaid" => self.render_mermaid(),
            "full" => self.render_full(),
            _ => self.render_summary(),
        }
    }

    fn render_summary(&self) -> String {
        let mut lines = vec!["# Grafo da Wiki\n".to_string()];
        lines.push(format!("- Páginas (nós): {}", self.node_count()));
        lines.push(format!("- Links (arestas): {}", self.edge_count()));
        lines.push(format!("- Páginas órfãs: {}", self.orphans().len()));
        lines.push(format!("- Links quebrados: {}", self.broken_links().len()));
        lines.push(String::new());
        lines.push("## Hubs (maior conectividade)\n".into());

        let hubs = self.hubs();
        let top: Vec<&HubEntry> = hubs
            .iter()
            .filter(|h| h.in_degree + h.out_degree > 0)
            .take(10)
            .collect();
        if top.is_empty() {
            lines.push("_Nenhum link entre páginas ainda._".into());
        } else {
            for (i, h) in top.iter().enumerate() {
                lines.push(format!(
                    "{}. `{}` — {} entradas, {} saídas",
                    i + 1,
                    h.slug,
                    h.in_degree,
                    h.out_degree
                ));
            }
        }
        lines.join("\n")
    }

    fn render_full(&self) -> String {
        let mut text = self.render_summary();
        text.push_str("\n\n## Adjacência\n");
        for slug in &self.nodes {
            text.push_str(&format!("\n### `{slug}`\n"));
            let out = self.outgoing.get(slug).cloned().unwrap_or_default();
            let inc = self.incoming.get(slug).cloned().unwrap_or_default();
            if out.is_empty() && inc.is_empty() {
                text.push_str("- _sem links_\n");
                continue;
            }
            if !out.is_empty() {
                let items: Vec<String> = out
                    .iter()
                    .map(|(t, k)| format!("`{t}` ({})", edge_kind_label(*k)))
                    .collect();
                text.push_str(&format!("- aponta para: {}\n", items.join(", ")));
            }
            if !inc.is_empty() {
                let items: Vec<String> = inc
                    .iter()
                    .map(|(s, k)| format!("`{s}` ({})", edge_kind_label(*k)))
                    .collect();
                text.push_str(&format!("- apontado por: {}\n", items.join(", ")));
            }
        }
        text
    }

    fn render_mermaid(&self) -> String {
        let ids: HashMap<&str, String> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), format!("n{i}")))
            .collect();

        let mut lines = vec!["```mermaid".to_string(), "graph LR".to_string()];
        for slug in &self.nodes {
            lines.push(format!("  {}[\"{}\"]", ids[slug.as_str()], slug));
        }

        let mut edge_lines: Vec<String> = Vec::new();
        for (from, targets) in &self.outgoing {
            let from_id = &ids[from.as_str()];
            for (to, kind) in targets {
                // só desenha a aresta se o alvo é um nó conhecido
                if let Some(to_id) = ids.get(to.as_str()) {
                    let arrow = match kind {
                        EdgeKind::Body => "-->",
                        EdgeKind::Related => "-.->",
                    };
                    edge_lines.push(format!("  {from_id} {arrow} {to_id}"));
                }
            }
        }
        edge_lines.sort();
        lines.extend(edge_lines);
        lines.push("```".into());
        lines.join("\n")
    }
}

// ── Extração de links ────────────────────────────────────────────────────────

/// Extrai slugs referenciados no conteúdo de uma página.
///
/// Reconhece duas sintaxes em paralelo (lenient — não exige nenhuma):
///   - `wiki://page/{slug}` — formato legado (markdown link ou bare).
///   - `[[slug]]` e `[[slug|Texto]]` — formato wikilink (compatível com Obsidian).
///
/// Ambas geram a mesma aresta no grafo; um slug que apareça pelos dois caminhos
/// é deduplicado na saída.
pub fn extract_wiki_page_links(content: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    extract_legacy_wiki_links(content, &mut links);
    extract_wikilink_braces(content, &mut links);

    links.sort();
    links.dedup();
    links
}

/// Extrai slugs no formato legado `wiki://page/{slug}` (markdown link ou bare).
fn extract_legacy_wiki_links(content: &str, out: &mut Vec<String>) {
    let prefix = "wiki://page/";
    let mut rest = content;

    while let Some(pos) = rest.find(prefix) {
        rest = &rest[pos + prefix.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
            .unwrap_or(rest.len());
        // slug não pode terminar com '.' (validate_slug rejeita) — strip de pontuação final
        let slug = rest[..end].trim_end_matches('.');
        if !slug.is_empty() {
            out.push(slug.to_string());
        }
        if end >= rest.len() {
            break;
        }
        rest = &rest[end..];
    }
}

/// Extrai slugs no formato wikilink `[[slug]]` ou `[[slug|Texto]]`.
///
/// O alvo (parte antes do `|`) deve ser um slug válido (alfanumérico + `-_.`).
/// Não cruza newlines — wikilinks precisam estar na mesma linha.
fn extract_wikilink_braces(content: &str, out: &mut Vec<String>) {
    let mut rest = content;

    while let Some(pos) = rest.find("[[") {
        rest = &rest[pos + 2..];
        // Procura fechamento `]]` ou newline (descarta quebra de linha — não é wikilink).
        let Some(close) = rest.find("]]") else {
            break;
        };
        let inner = &rest[..close];
        if inner.contains('\n') {
            // não cruza linhas — avança para depois do `[[` e tenta de novo
            continue;
        }

        // Separa alvo de alias (se houver `|`).
        let target = match inner.find('|') {
            Some(p) => &inner[..p],
            None => inner,
        };
        let target = target.trim();

        // Valida o alvo como slug (mesma regra do `wiki://page/`).
        let is_valid_slug = !target.is_empty()
            && !target.ends_with('.')
            && target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if is_valid_slug {
            out.push(target.to_string());
        }

        rest = &rest[close + 2..];
    }
}

// ── Sugestão de links ────────────────────────────────────────────────────────

/// Sugere links entre páginas ainda não conectadas, combinando similaridade de
/// conteúdo (Jaccard) com boosts de mesmo `project` e tags em comum.
///
/// Se `focus` for `Some`, retorna apenas pares envolvendo essa página.
pub async fn suggest_links(
    file_manager: &WikiFileManager,
    graph: &WikiGraph,
    focus: Option<&str>,
    min_score: f32,
    max_suggestions: usize,
) -> anyhow::Result<Vec<LinkSuggestion>> {
    struct PageData {
        slug: String,
        tokens: HashSet<String>,
        project: Option<String>,
        tags: Vec<String>,
    }

    // carrega conteúdo + frontmatter de cada página (exceto a `index` gerada)
    let mut pages: Vec<PageData> = Vec::new();
    for slug in &graph.nodes {
        if slug == GENERATED_INDEX_SLUG {
            continue;
        }
        let content = match file_manager.read_page(slug).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let fm = crate::frontmatter::parse_frontmatter(&content);
        pages.push(PageData {
            slug: slug.clone(),
            tokens: crate::lint::tokenize(&content),
            project: fm
                .as_ref()
                .and_then(|f| f.project.clone())
                .map(|p| p.to_lowercase()),
            tags: fm
                .as_ref()
                .map(|f| f.tags.iter().map(|t| t.to_lowercase()).collect())
                .unwrap_or_default(),
        });
    }

    let mut suggestions: Vec<LinkSuggestion> = Vec::new();
    for i in 0..pages.len() {
        for j in (i + 1)..pages.len() {
            let a = &pages[i];
            let b = &pages[j];

            if let Some(f) = focus {
                if a.slug != f && b.slug != f {
                    continue;
                }
            }
            if graph.has_edge(&a.slug, &b.slug) {
                continue;
            }

            let sim = crate::lint::jaccard(&a.tokens, &b.tokens);
            let mut score = sim;
            let mut reasons: Vec<String> = Vec::new();
            if sim > 0.0 {
                reasons.push(format!("similaridade de conteúdo {:.0}%", sim * 100.0));
            }

            if let (Some(pa), Some(pb)) = (&a.project, &b.project) {
                if pa == pb {
                    score += SAME_PROJECT_BOOST;
                    reasons.push(format!("mesmo projeto '{pa}'"));
                }
            }

            let shared_tags: Vec<String> =
                a.tags.iter().filter(|t| b.tags.contains(t)).cloned().collect();
            if !shared_tags.is_empty() {
                let boost = (shared_tags.len() as f32 * SHARED_TAG_BOOST).min(MAX_TAG_BOOST);
                score += boost;
                reasons.push(format!("tags em comum: {}", shared_tags.join(", ")));
            }

            if score >= min_score && !reasons.is_empty() {
                let (from, to) = match focus {
                    Some(f) if b.slug == f => (b.slug.clone(), a.slug.clone()),
                    _ => (a.slug.clone(), b.slug.clone()),
                };
                suggestions.push(LinkSuggestion {
                    from,
                    to,
                    score,
                    reasons,
                });
            }
        }
    }

    suggestions.sort_by(|x, y| {
        y.score
            .partial_cmp(&x.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.from.cmp(&y.from))
            .then(x.to.cmp(&y.to))
    });
    suggestions.truncate(max_suggestions);
    Ok(suggestions)
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::WikiFileManager;
    use std::sync::Arc;
    use tempfile::TempDir;

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

    // ── wikilink syntax (Obsidian-compatible) ────────────────────────────────

    #[test]
    fn test_extract_wikilink_simple() {
        let content = "veja [[getting-started]] para começar";
        assert_eq!(extract_wiki_page_links(content), vec!["getting-started"]);
    }

    #[test]
    fn test_extract_wikilink_with_alias() {
        let content = "veja [[getting-started|este guia]] para começar";
        assert_eq!(extract_wiki_page_links(content), vec!["getting-started"]);
    }

    #[test]
    fn test_extract_wikilink_multiple() {
        let content = "links: [[home]] e [[api-reference|API]] aqui";
        assert_eq!(
            extract_wiki_page_links(content),
            vec!["api-reference", "home"]
        );
    }

    #[test]
    fn test_extract_wikilink_mixed_with_legacy() {
        // ambos os formatos no mesmo documento devem ser reconhecidos
        let content = "antigo wiki://page/old-style e novo [[new-style]] aqui";
        assert_eq!(
            extract_wiki_page_links(content),
            vec!["new-style", "old-style"]
        );
    }

    #[test]
    fn test_extract_wikilink_dedup_across_formats() {
        // mesma página citada nos dois formatos vira uma só aresta
        let content = "wiki://page/home e [[home]] e [[home|Início]]";
        assert_eq!(extract_wiki_page_links(content), vec!["home"]);
    }

    #[test]
    fn test_extract_wikilink_ignores_newline() {
        // colchetes que cruzam linha não são wikilinks
        let content = "[[\nhome\n]] não é link";
        assert!(extract_wiki_page_links(content).is_empty());
    }

    #[test]
    fn test_extract_wikilink_rejects_invalid_slug() {
        // espaços e caracteres especiais no alvo invalidam o wikilink
        let content = "[[com espaço]] e [[com/barra]]";
        assert!(extract_wiki_page_links(content).is_empty());
    }

    #[test]
    fn test_extract_wikilink_trims_target() {
        let content = "[[  spaced-slug  |alias]]";
        assert_eq!(extract_wiki_page_links(content), vec!["spaced-slug"]);
    }

    // ── build / backlinks ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_build_empty_graph() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        let graph = WikiGraph::build(&fm).await.unwrap();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[tokio::test]
    async fn test_build_body_links_and_backlinks() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "veja wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        let bl = graph.backlinks("about");
        assert_eq!(bl, vec![("home", EdgeKind::Body)]);
        assert!(graph.backlinks("home").is_empty());
    }

    #[tokio::test]
    async fn test_build_related_frontmatter_creates_edge() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page(
            "home",
            "---\ntype: note\nrelated:\n  - about\n---\n# Home",
        )
        .await
        .unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let bl = graph.backlinks("about");
        assert_eq!(bl, vec![("home", EdgeKind::Related)]);
    }

    #[tokio::test]
    async fn test_body_link_wins_over_related() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page(
            "home",
            "---\ntype: note\nrelated:\n  - about\n---\n# Home\n\nwiki://page/about",
        )
        .await
        .unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.backlinks("about"), vec![("home", EdgeKind::Body)]);
    }

    #[tokio::test]
    async fn test_self_link_ignored() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "eu mesmo: wiki://page/home").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        assert_eq!(graph.edge_count(), 0);
    }

    // ── orphans ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_orphans_basic() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "veja wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();
        fm.write_page("lonely", "sozinha").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let orphans = graph.orphans();
        assert!(orphans.contains(&"home".to_string()));
        assert!(orphans.contains(&"lonely".to_string()));
        assert!(!orphans.contains(&"about".to_string()));
    }

    #[tokio::test]
    async fn test_orphans_ignores_generated_index() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        // a página `index` linka todas as outras — não deve "salvar" ninguém
        fm.write_page("index", "wiki://page/about wiki://page/home")
            .await
            .unwrap();
        fm.write_page("home", "veja wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre nós").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let orphans = graph.orphans();
        // home só recebe link da index → ainda é órfã
        assert!(orphans.contains(&"home".to_string()));
        // about recebe link real de home → não é órfã
        assert!(!orphans.contains(&"about".to_string()));
        // a própria index nunca aparece
        assert!(!orphans.contains(&"index".to_string()));
    }

    // ── broken_links ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_broken_links() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "veja wiki://page/inexistente").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let broken = graph.broken_links();
        assert_eq!(broken, vec![("home".to_string(), "inexistente".to_string())]);
    }

    // ── related ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_related_classifies_relations() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        // home <-> mutual ; home -> target ; source -> home
        fm.write_page(
            "home",
            "wiki://page/mutual wiki://page/target",
        )
        .await
        .unwrap();
        fm.write_page("mutual", "wiki://page/home").await.unwrap();
        fm.write_page("target", "alvo").await.unwrap();
        fm.write_page("source", "wiki://page/home").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let related = graph.related("home");
        let by_slug: HashMap<&str, Relation> =
            related.iter().map(|r| (r.slug.as_str(), r.relation)).collect();

        assert_eq!(by_slug.get("mutual"), Some(&Relation::Bidirectional));
        assert_eq!(by_slug.get("target"), Some(&Relation::LinksTo));
        assert_eq!(by_slug.get("source"), Some(&Relation::LinkedFrom));
        // bidirecional vem primeiro (rank menor)
        assert_eq!(related[0].slug, "mutual");
    }

    #[tokio::test]
    async fn test_related_declared_via_frontmatter() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page(
            "home",
            "---\ntype: note\nrelated:\n  - declared\n---\n# Home",
        )
        .await
        .unwrap();
        fm.write_page("declared", "página declarada").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let related = graph.related("home");
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].slug, "declared");
        assert_eq!(related[0].relation, Relation::Declared);
    }

    // ── has_edge / hubs ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_has_edge_either_direction() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("a", "wiki://page/b").await.unwrap();
        fm.write_page("b", "página b").await.unwrap();
        fm.write_page("c", "página c").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        assert!(graph.has_edge("a", "b"));
        assert!(graph.has_edge("b", "a"));
        assert!(!graph.has_edge("a", "c"));
    }

    #[tokio::test]
    async fn test_hubs_sorted_by_degree() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("hub", "página central").await.unwrap();
        fm.write_page("a", "wiki://page/hub").await.unwrap();
        fm.write_page("b", "wiki://page/hub").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let hubs = graph.hubs();
        assert_eq!(hubs[0].slug, "hub");
        assert_eq!(hubs[0].in_degree, 2);
    }

    // ── render ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_render_summary() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let md = graph.render("summary");
        assert!(md.contains("# Grafo da Wiki"));
        assert!(md.contains("- Páginas (nós): 2"));
        assert!(md.contains("- Links (arestas): 1"));
    }

    #[tokio::test]
    async fn test_render_mermaid() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;
        fm.write_page("home", "wiki://page/about").await.unwrap();
        fm.write_page("about", "sobre").await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let md = graph.render("mermaid");
        assert!(md.contains("```mermaid"));
        assert!(md.contains("graph LR"));
        assert!(md.contains("-->"));
    }

    // ── suggest_links ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_suggest_links_by_content_similarity() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;

        let words: Vec<String> = (0..40).map(|i| format!("palavra{i:03}")).collect();
        let shared = words.join(" ");
        fm.write_page("page-a", &shared).await.unwrap();
        fm.write_page("page-b", &shared).await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let suggestions = suggest_links(&fm, &graph, None, 0.15, 10).await.unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].from, "page-a");
        assert_eq!(suggestions[0].to, "page-b");
    }

    #[tokio::test]
    async fn test_suggest_links_skips_already_linked() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;

        let words: Vec<String> = (0..40).map(|i| format!("palavra{i:03}")).collect();
        let shared = words.join(" ");
        fm.write_page("page-a", &format!("{shared} wiki://page/page-b"))
            .await
            .unwrap();
        fm.write_page("page-b", &shared).await.unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let suggestions = suggest_links(&fm, &graph, None, 0.15, 10).await.unwrap();
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn test_suggest_links_focus_filter() {
        let dir = TempDir::new().unwrap();
        let fm = make_manager(dir.path().to_path_buf()).await;

        let words: Vec<String> = (0..40).map(|i| format!("palavra{i:03}")).collect();
        let shared = words.join(" ");
        fm.write_page("page-a", &shared).await.unwrap();
        fm.write_page("page-b", &shared).await.unwrap();
        fm.write_page("unrelated", "conteúdo totalmente diferente xyz")
            .await
            .unwrap();

        let graph = WikiGraph::build(&fm).await.unwrap();
        let suggestions = suggest_links(&fm, &graph, Some("page-a"), 0.15, 10)
            .await
            .unwrap();
        assert!(suggestions.iter().all(|s| s.from == "page-a" || s.to == "page-a"));
    }
}
