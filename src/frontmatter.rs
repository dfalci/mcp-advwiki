// ── Módulo de Frontmatter YAML ───────────────────────────────────────────────
//
// Parseia e manipula metadados estruturados no início de páginas Markdown.
//
// Formato esperado (bloco delimitado por `---`):
//
//   ---
//   type: service
//   project: my-project
//   status: active
//   tags:
//     - backend
//     - api
//   ---
//
//   # Título da Página
//
// Responsabilidades:
//   - parse_frontmatter      → extrai campos estruturados
//   - strip_frontmatter      → retorna o conteúdo sem o bloco YAML
//   - update_date_fields     → injeta/atualiza created_at e updated_at

use serde::{Deserialize, Serialize};

/// Metadados estruturados de uma página da Wiki.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageFrontmatter {
    /// Tipo semântico da página.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub page_type: Option<String>,
    /// Projeto ao qual a página pertence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Status da página.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Data de criação (ISO 8601, gerenciada pelo servidor).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Data de última atualização (ISO 8601, gerenciada pelo servidor).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Nível de confiança no conteúdo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// URIs de raw sources que embasam esta página.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Slugs de páginas relacionadas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<String>,
    /// Tags livres.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Responsável pela página (opcional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

// ── API pública ──────────────────────────────────────────────────────────────

/// extrai e parseia o bloco frontmatter de um conteúdo markdown.
/// retorna `None` se não houver frontmatter válido.
pub fn parse_frontmatter(content: &str) -> Option<PageFrontmatter> {
    let body = extract_body(content)?;
    serde_yaml::from_str(body).ok()
}

/// retorna o conteúdo sem o bloco frontmatter (incluindo os delimitadores `---`).
/// se não houver frontmatter, retorna o conteúdo original intacto.
pub fn strip_frontmatter(content: &str) -> &str {
    let Some((_, rest)) = split_frontmatter(content) else {
        return content;
    };
    rest
}

/// injeta ou atualiza `updated_at` no frontmatter com a data `today` (formato `YYYY-MM-DD`).
/// se `is_new_page` for verdadeiro e `created_at` não existir, também o injeta.
/// se não houver frontmatter no conteúdo, retorna o conteúdo sem alterações.
pub fn update_date_fields(content: &str, today: &str, is_new_page: bool) -> String {
    let Some((fm_body, rest)) = split_frontmatter(content) else {
        return content.to_string();
    };

    let fm_body = upsert_scalar(fm_body, "updated_at", today);

    let fm_body = if is_new_page && !has_key(&fm_body, "created_at") {
        upsert_scalar(&fm_body, "created_at", today)
    } else {
        fm_body
    };

    format!("---\n{fm_body}\n---\n{rest}")
}

// ── Helpers internos ─────────────────────────────────────────────────────────

/// extrai o texto bruto entre os delimitadores `---`.
fn extract_body(content: &str) -> Option<&str> {
    let (body, _) = split_frontmatter(content)?;
    Some(body)
}

/// divide o conteúdo em `(frontmatter_body, rest_after_closing_delimiter)`.
/// retorna `None` se o conteúdo não começar com `---\n`.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix("---\n")?;
    let close = content.find("\n---\n").or_else(|| {
        // trata EOF: `\n---` sem newline final
        if content.ends_with("\n---") {
            Some(content.len() - 4)
        } else {
            None
        }
    })?;
    let body = &content[..close];

    // Valida que o corpo é um YAML mapping válido e não um bloco de Markdown
    // acidental (ex: linha horizontal `---` no início da página).
    // Um mapping YAML tem ao menos uma linha no formato `key: value` ou `key:`.
    if !is_yaml_mapping(body) {
        return None;
    }

    let rest = if close + 5 <= content.len() {
        &content[close + 5..] // skip "\n---\n"
    } else {
        ""
    };
    Some((body, rest))
}

/// verifica se o texto pode ser um YAML mapping (ao menos uma linha `key:` ou `key: value`).
/// descarta thematic breaks do Markdown e outros blocos que começam com `---`.
fn is_yaml_mapping(body: &str) -> bool {
    if body.trim().is_empty() {
        return false;
    }
    // Toda linha não vazia deve ser: uma chave (`key:` ou `key: value`),
    // um item de lista (`- value`) ou continuação indentada de valor.
    // Qualquer linha que comece com `#` (heading) descarta o bloco inteiro.
    let mut has_key = false;
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            // cabeçalho Markdown — definitivamente não é frontmatter
            return false;
        }
        // linha de chave YAML: começa com identificador seguido de `:`
        let looks_like_key = line
            .splitn(2, ':')
            .next()
            .map(|k| {
                let k = k.trim();
                !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            })
            .unwrap_or(false);

        if looks_like_key && line.contains(':') {
            has_key = true;
        } else if !line.starts_with('-') && !line.starts_with(' ') && !line.starts_with('\t') {
            // linha que não é chave, lista nem continuação indentada
            return false;
        }
    }
    has_key
}

/// verifica se a chave `key` existe como campo de primeiro nível no frontmatter.
fn has_key(body: &str, key: &str) -> bool {
    let prefix = format!("{key}:");
    body.lines()
        .any(|line| line == prefix.trim_end() || line.starts_with(&format!("{key}: ")) || line.starts_with(&format!("{key}:\t")))
}

/// insere ou substitui um campo escalar `key: "value"` no corpo do frontmatter.
/// preserva todos os outros campos e a ordem original.
fn upsert_scalar(body: &str, key: &str, value: &str) -> String {
    let key_prefix = format!("{key}:");
    let new_line = format!("{key}: \"{value}\"");
    let mut found = false;

    let lines: Vec<String> = body
        .lines()
        .map(|line| {
            if !found
                && (line == key_prefix.trim_end()
                    || line.starts_with(&format!("{key}: "))
                    || line.starts_with(&format!("{key}:\t")))
            {
                found = true;
                new_line.clone()
            } else {
                line.to_string()
            }
        })
        .collect();

    let mut result = lines.join("\n");
    if !found {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&new_line);
    }
    result
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\ntype: service\nproject: my-project\nstatus: active\ntags:\n  - backend\n  - api\n---\n\n# Título\n\nConteúdo aqui.";

    // ── parse_frontmatter ────────────────────────────────────────────────────

    #[test]
    fn test_parse_frontmatter_basic() {
        let fm = parse_frontmatter(SAMPLE).unwrap();
        assert_eq!(fm.page_type.as_deref(), Some("service"));
        assert_eq!(fm.project.as_deref(), Some("my-project"));
        assert_eq!(fm.status.as_deref(), Some("active"));
        assert_eq!(fm.tags, vec!["backend", "api"]);
    }

    #[test]
    fn test_parse_frontmatter_none_when_no_block() {
        assert!(parse_frontmatter("# Just a title\n\nno frontmatter").is_none());
    }

    #[test]
    fn test_parse_frontmatter_none_when_malformed() {
        assert!(parse_frontmatter("---\nno closing delimiter").is_none());
    }

    #[test]
    fn test_parse_frontmatter_sources_and_related() {
        let content = "---\nsources:\n  - raw://source/abc\nrelated:\n  - other-page\n---\nconteúdo";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.sources, vec!["raw://source/abc"]);
        assert_eq!(fm.related, vec!["other-page"]);
    }

    #[test]
    fn test_parse_frontmatter_defaults_empty_vecs() {
        let content = "---\ntype: note\n---\nconteúdo";
        let fm = parse_frontmatter(content).unwrap();
        assert!(fm.sources.is_empty());
        assert!(fm.related.is_empty());
        assert!(fm.tags.is_empty());
    }

    #[test]
    fn test_parse_frontmatter_ignores_markdown_thematic_break() {
        // página pré-existente que começa com linha horizontal (thematic break)
        let content = "---\n\n## Introdução\n\nTexto.\n\n---\n\n## Seção 2";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_ignores_heading_inside_dashes() {
        // conteúdo com heading entre delimitadores — não é frontmatter
        let content = "---\n# Título\nTexto qualquer.\n---\nresto";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_ignores_empty_body() {
        let content = "---\n\n---\nresto";
        assert!(parse_frontmatter(content).is_none());
    }

    // ── strip_frontmatter ────────────────────────────────────────────────────

    #[test]
    fn test_strip_frontmatter_removes_block() {
        let stripped = strip_frontmatter(SAMPLE);
        assert!(!stripped.contains("type: service"));
        assert!(stripped.contains("# Título"));
        assert!(stripped.contains("Conteúdo aqui."));
    }

    #[test]
    fn test_strip_frontmatter_no_block_unchanged() {
        let content = "# Just a title\n\nno frontmatter";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_strip_frontmatter_empty_page() {
        let content = "---\ntype: note\n---\n";
        let stripped = strip_frontmatter(content);
        assert!(stripped.is_empty() || stripped == "\n");
    }

    // ── update_date_fields ───────────────────────────────────────────────────

    #[test]
    fn test_update_date_fields_injects_updated_at() {
        let content = "---\ntype: service\n---\n\nConteúdo.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert!(result.contains("updated_at: \"2026-05-15\""));
        assert!(result.contains("type: service"));
        assert!(result.contains("Conteúdo."));
    }

    #[test]
    fn test_update_date_fields_replaces_existing_updated_at() {
        let content = "---\ntype: service\nupdated_at: \"2026-01-01\"\n---\n\nConteúdo.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert!(result.contains("updated_at: \"2026-05-15\""));
        assert!(!result.contains("2026-01-01"));
    }

    #[test]
    fn test_update_date_fields_injects_created_at_for_new_page() {
        let content = "---\ntype: service\n---\n\nConteúdo.";
        let result = update_date_fields(content, "2026-05-15", true);
        assert!(result.contains("created_at: \"2026-05-15\""));
        assert!(result.contains("updated_at: \"2026-05-15\""));
    }

    #[test]
    fn test_update_date_fields_does_not_overwrite_created_at_on_existing_page() {
        let content = "---\ntype: service\ncreated_at: \"2026-01-01\"\n---\n\nConteúdo.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert!(result.contains("created_at: \"2026-01-01\""));
    }

    #[test]
    fn test_update_date_fields_no_frontmatter_unchanged() {
        let content = "# Título\n\nSem frontmatter.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert_eq!(result, content);
    }

    #[test]
    fn test_update_date_fields_thematic_break_unchanged() {
        // página pré-existente com linha horizontal — NÃO deve ser modificada
        let content = "---\n\n## Intro\n\nTexto.\n\n---\n\n## Seção 2";
        let result = update_date_fields(content, "2026-05-15", false);
        assert_eq!(result, content);
    }

    #[test]
    fn test_update_date_fields_preserves_other_fields() {
        let content = "---\ntype: decision\nproject: meu-projeto\nstatus: active\n---\nConteúdo.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert!(result.contains("type: decision"));
        assert!(result.contains("project: meu-projeto"));
        assert!(result.contains("status: active"));
    }

    #[test]
    fn test_update_date_fields_preserves_content_after_delimiter() {
        let content = "---\ntype: note\n---\n\n# Minha Página\n\nTexto importante aqui.";
        let result = update_date_fields(content, "2026-05-15", false);
        assert!(result.contains("# Minha Página"));
        assert!(result.contains("Texto importante aqui."));
    }

    // ── upsert_scalar ────────────────────────────────────────────────────────

    #[test]
    fn test_upsert_scalar_adds_new_key() {
        let body = "type: service\nproject: foo";
        let result = upsert_scalar(body, "status", "active");
        assert!(result.contains("status: \"active\""));
        assert!(result.contains("type: service"));
    }

    #[test]
    fn test_upsert_scalar_replaces_existing_key() {
        let body = "type: service\nstatus: draft";
        let result = upsert_scalar(body, "status", "active");
        assert!(result.contains("status: \"active\""));
        assert!(!result.contains("status: draft"));
    }

    #[test]
    fn test_upsert_scalar_does_not_duplicate() {
        let body = "status: draft";
        let result = upsert_scalar(body, "status", "active");
        assert_eq!(result.matches("status:").count(), 1);
    }
}
