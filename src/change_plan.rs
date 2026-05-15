// ── Módulo de Planejamento de Alterações (item 5 do roadmap) ────────────────
//
// Permite que uma alteração de página seja proposta e revisada com diff
// antes de ser gravada. As propostas são persistidas em
// `.advwiki/proposals/<id>.json` e aplicadas depois pelo seu id.

use serde::{Deserialize, Serialize};

/// Estado de uma proposta ainda não aplicada.
pub const STATUS_PENDING: &str = "pending";
/// Estado de uma proposta já aplicada à página.
pub const STATUS_APPLIED: &str = "applied";

/// Linhas de contexto exibidas ao redor de cada mudança no diff.
const CONTEXT: usize = 3;

/// Proposta de alteração de uma página da wiki, persistida em disco.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageProposal {
    /// Identificador único da proposta (também é o nome do arquivo JSON).
    pub proposal_id: String,
    /// Slug da página alvo.
    pub slug: String,
    /// Justificativa da alteração.
    pub reason: String,
    /// Timestamp ISO 8601 de criação da proposta.
    pub created_at: String,
    /// Se a página já existia no momento da proposta.
    pub base_page_exists: bool,
    /// Hash MD5 do conteúdo atual da página no momento da proposta
    /// (`None` quando a página ainda não existe).
    pub base_hash: Option<String>,
    /// Conteúdo final proposto (já com os campos de data aplicados).
    pub proposed_content: String,
    /// Estado da proposta: `pending` ou `applied`.
    pub status: String,
    /// Timestamp ISO 8601 da aplicação (preenchido ao aplicar).
    #[serde(default)]
    pub applied_at: Option<String>,
}

impl PageProposal {
    /// Cria uma nova proposta no estado `pending`.
    pub fn new(
        proposal_id: String,
        slug: String,
        reason: String,
        base_page_exists: bool,
        base_hash: Option<String>,
        proposed_content: String,
    ) -> Self {
        Self {
            proposal_id,
            slug,
            reason,
            created_at: chrono::Utc::now().to_rfc3339(),
            base_page_exists,
            base_hash,
            proposed_content,
            status: STATUS_PENDING.to_string(),
            applied_at: None,
        }
    }

    /// `true` se a proposta ainda não foi aplicada.
    pub fn is_pending(&self) -> bool {
        self.status == STATUS_PENDING
    }
}

/// Calcula o hash MD5 (hex) de um conteúdo textual.
///
/// Usado para detectar se a página base foi modificada entre a criação
/// da proposta e a sua aplicação.
pub fn content_hash(content: &str) -> String {
    use md5::Digest;
    md5::Md5::digest(content.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Gera um id de proposta legível e único: `<slug>-<YYYYMMDDHHMMSS>-<6 hex>`.
///
/// O id é usado como nome de arquivo, então mantém apenas os caracteres
/// (já validados) do slug, dígitos e hexadecimais.
pub fn new_proposal_id(slug: &str) -> String {
    let now = chrono::Utc::now();
    let ts = now.format("%Y%m%d%H%M%S");
    let nanos = now.timestamp_nanos_opt().unwrap_or_default();
    let short: String = content_hash(&format!("{slug}:{nanos}"))
        .chars()
        .take(6)
        .collect();
    format!("{slug}-{ts}-{short}")
}

// ── Diff de linhas (estilo unified) ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Computa a sequência de operações que transforma `base` em `proposed`,
/// usando a maior subsequência comum (LCS) de linhas.
fn diff_ops<'a>(base: &[&'a str], proposed: &[&'a str]) -> Vec<Op<'a>> {
    let n = base.len();
    let m = proposed.len();

    // Tabela de comprimento da LCS (sufixos).
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if base[i] == proposed[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    // Backtrack para montar a sequência de operações.
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if base[i] == proposed[j] {
            ops.push(Op::Equal(base[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(Op::Delete(base[i]));
            i += 1;
        } else {
            ops.push(Op::Insert(proposed[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(base[i]));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(proposed[j]));
        j += 1;
    }
    ops
}

/// Conta linhas adicionadas e removidas entre `base` e `proposed`.
pub fn diff_stats(base: &str, proposed: &str) -> (usize, usize) {
    let base_lines: Vec<&str> = base.lines().collect();
    let proposed_lines: Vec<&str> = proposed.lines().collect();
    let ops = diff_ops(&base_lines, &proposed_lines);

    let mut added = 0;
    let mut removed = 0;
    for op in &ops {
        match op {
            Op::Insert(_) => added += 1,
            Op::Delete(_) => removed += 1,
            Op::Equal(_) => {}
        }
    }
    (added, removed)
}

/// Renderiza um diff estilo "unified" entre `base` e `proposed`.
///
/// Mostra apenas as regiões alteradas, com até `CONTEXT` linhas de contexto
/// ao redor de cada mudança. Retorna string vazia quando não há diferenças.
pub fn render_unified_diff(base: &str, proposed: &str) -> String {
    let base_lines: Vec<&str> = base.lines().collect();
    let proposed_lines: Vec<&str> = proposed.lines().collect();
    let ops = diff_ops(&base_lines, &proposed_lines);

    let n = ops.len();
    if ops.iter().all(|op| matches!(op, Op::Equal(_))) {
        return String::new();
    }

    // Marca quais operações entram em uma região de contexto a exibir.
    let mut show = vec![false; n];
    for (idx, op) in ops.iter().enumerate() {
        if !matches!(op, Op::Equal(_)) {
            let lo = idx.saturating_sub(CONTEXT);
            let hi = (idx + CONTEXT + 1).min(n);
            for flag in show.iter_mut().take(hi).skip(lo) {
                *flag = true;
            }
        }
    }

    let mut out = String::new();
    let mut idx = 0;
    let mut base_ln = 1usize;
    let mut prop_ln = 1usize;

    while idx < n {
        if !show[idx] {
            match ops[idx] {
                Op::Equal(_) => {
                    base_ln += 1;
                    prop_ln += 1;
                }
                Op::Delete(_) => base_ln += 1,
                Op::Insert(_) => prop_ln += 1,
            }
            idx += 1;
            continue;
        }

        // Início de um hunk: consome o intervalo contíguo marcado.
        let (hunk_base_start, hunk_prop_start) = (base_ln, prop_ln);
        let mut base_count = 0;
        let mut prop_count = 0;
        let mut body = String::new();

        while idx < n && show[idx] {
            match ops[idx] {
                Op::Equal(line) => {
                    body.push_str(&format!(" {line}\n"));
                    base_count += 1;
                    prop_count += 1;
                    base_ln += 1;
                    prop_ln += 1;
                }
                Op::Delete(line) => {
                    body.push_str(&format!("-{line}\n"));
                    base_count += 1;
                    base_ln += 1;
                }
                Op::Insert(line) => {
                    body.push_str(&format!("+{line}\n"));
                    prop_count += 1;
                    prop_ln += 1;
                }
            }
            idx += 1;
        }

        out.push_str(&format!(
            "@@ -{hunk_base_start},{base_count} +{hunk_prop_start},{prop_count} @@\n"
        ));
        out.push_str(&body);
    }

    out
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_hash_is_deterministic() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
    }

    #[test]
    fn test_content_hash_differs_for_different_input() {
        assert_ne!(content_hash("hello"), content_hash("world"));
    }

    #[test]
    fn test_content_hash_is_hex_md5_length() {
        assert_eq!(content_hash("anything").len(), 32);
        assert!(content_hash("anything").chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_new_proposal_id_starts_with_slug() {
        let id = new_proposal_id("getting-started");
        assert!(id.starts_with("getting-started-"), "id inesperado: {id}");
    }

    #[test]
    fn test_new_proposal_id_only_safe_chars() {
        let id = new_proposal_id("api_v2");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "id com caractere inseguro: {id}"
        );
    }

    #[test]
    fn test_page_proposal_is_pending_by_default() {
        let p = PageProposal::new(
            "p1".into(),
            "home".into(),
            "motivo".into(),
            false,
            None,
            "conteúdo".into(),
        );
        assert!(p.is_pending());
        assert_eq!(p.status, STATUS_PENDING);
        assert!(p.applied_at.is_none());
    }

    #[test]
    fn test_page_proposal_serde_roundtrip() {
        let p = PageProposal::new(
            "home-20260515-abc123".into(),
            "home".into(),
            "ajuste".into(),
            true,
            Some("deadbeef".into()),
            "# Home\n".into(),
        );
        let json = serde_json::to_string(&p).expect("serialize");
        let back: PageProposal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn test_diff_stats_identical_is_zero() {
        assert_eq!(diff_stats("a\nb\nc", "a\nb\nc"), (0, 0));
    }

    #[test]
    fn test_diff_stats_pure_insert() {
        assert_eq!(diff_stats("a\nb", "a\nx\ny\nb"), (2, 0));
    }

    #[test]
    fn test_diff_stats_pure_delete() {
        assert_eq!(diff_stats("a\nx\ny\nb", "a\nb"), (0, 2));
    }

    #[test]
    fn test_diff_stats_modification() {
        // Uma linha trocada conta como 1 remoção + 1 inserção.
        assert_eq!(diff_stats("a\nb\nc", "a\nB\nc"), (1, 1));
    }

    #[test]
    fn test_render_diff_identical_is_empty() {
        assert_eq!(render_unified_diff("a\nb\nc", "a\nb\nc"), "");
    }

    #[test]
    fn test_render_diff_new_page_shows_all_as_insert() {
        let diff = render_unified_diff("", "linha 1\nlinha 2");
        assert!(diff.contains("+linha 1"), "diff:\n{diff}");
        assert!(diff.contains("+linha 2"), "diff:\n{diff}");
        assert!(!diff.contains("-linha"), "diff:\n{diff}");
    }

    #[test]
    fn test_render_diff_modification_shows_both_sides() {
        let diff = render_unified_diff("# Título\ntexto antigo\nfim", "# Título\ntexto novo\nfim");
        assert!(diff.contains("-texto antigo"), "diff:\n{diff}");
        assert!(diff.contains("+texto novo"), "diff:\n{diff}");
        // contexto inalterado é prefixado com espaço
        assert!(diff.contains(" # Título"), "diff:\n{diff}");
        assert!(diff.contains("@@"), "diff:\n{diff}");
    }

    #[test]
    fn test_render_diff_omits_far_context() {
        let base: String = (0..40).map(|i| format!("alfa{i}\n")).collect();
        let mut proposed_lines: Vec<String> = (0..40).map(|i| format!("alfa{i}")).collect();
        proposed_lines[20] = "alfa20-alterada".to_string();
        let proposed = proposed_lines.join("\n") + "\n";

        let diff = render_unified_diff(&base, &proposed);
        // a mudança aparece, com contexto próximo
        assert!(diff.contains("+alfa20-alterada"), "diff:\n{diff}");
        assert!(diff.contains(" alfa18"), "diff:\n{diff}");
        // linhas distantes não aparecem
        assert!(!diff.contains("alfa5"), "diff:\n{diff}");
        assert!(!diff.contains("alfa35"), "diff:\n{diff}");
    }
}
