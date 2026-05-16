// ── Módulo de Claims Rastreáveis ─────────────────────────────────────────────
//
// Parseia o bloco `## Claims` do corpo de uma página markdown. Cada claim é uma
// afirmação com origem, confiança e data de verificação — permitindo rastrear
// de onde veio uma informação e quando foi checada pela última vez.
//
// Formato reconhecido (no corpo da página, fora do frontmatter):
//
//   ## Claims
//
//   - A plataforma usa três escopos de vector store.
//     - Source: `wiki://page/decisao-vetores`
//     - Confidence: high
//     - Last verified: 2026-05-11
//
// Os labels de campo são bilíngues (EN/PT). Exposto via tools MCP:
// find_claims, find_claims_without_source, find_conflicting_claims, verify_claim.

/// Cabeçalhos de seção reconhecidos para o bloco de claims.
const CLAIMS_HEADERS: &[&str] = &["## Claims", "## Afirmações", "## Afirmacoes"];

// ── Tipos ────────────────────────────────────────────────────────────────────

/// Uma afirmação rastreável extraída do bloco `## Claims`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claim {
    /// Texto da afirmação.
    pub text: String,
    /// Origem da afirmação (URI `wiki://` / `raw://` ou descrição).
    pub source: Option<String>,
    /// Nível de confiança declarado.
    pub confidence: Option<String>,
    /// Data da última verificação.
    pub last_verified: Option<String>,
}

/// Campo de metadado de um claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimField {
    Source,
    Confidence,
    LastVerified,
}

/// Claim com a posição das suas linhas no conteúdo — usado para edição.
struct ParsedClaim {
    claim: Claim,
    /// índice (0-based) da linha do bullet `- <texto>`.
    text_line: usize,
    /// índice (0-based, exclusivo) onde o bloco do claim termina.
    block_end: usize,
}

// ── API pública ──────────────────────────────────────────────────────────────

/// Extrai os claims do bloco `## Claims` de uma página. Lista vazia se a página
/// não tiver a seção.
pub fn parse_claims(content: &str) -> Vec<Claim> {
    parse_claims_detailed(content)
        .into_iter()
        .map(|p| p.claim)
        .collect()
}

/// Atualiza o campo `Last verified` do claim de índice `index` (0-based) para
/// `date`. Substitui o valor se o campo existir; insere um novo sub-bullet se
/// não existir. Retorna o conteúdo modificado.
pub fn set_last_verified(content: &str, index: usize, date: &str) -> Result<String, String> {
    let parsed = parse_claims_detailed(content);
    if parsed.is_empty() {
        return Err("a página não tem uma seção `## Claims`".to_string());
    }
    let claim = parsed.get(index).ok_or_else(|| {
        format!(
            "índice de claim inválido: {} (a página tem {} claim(s))",
            index + 1,
            parsed.len()
        )
    })?;

    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    // Varre o bloco do claim procurando o sub-bullet `Last verified` e o último
    // sub-bullet (ponto de inserção, caso o campo não exista).
    let mut last_verified_line: Option<usize> = None;
    let mut last_subbullet: Option<usize> = None;
    let mut indent = "  ".to_string();

    for i in claim.text_line + 1..claim.block_end.min(lines.len()) {
        let line = &lines[i];
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- ") {
            continue;
        }
        indent = line[..line.len() - trimmed.len()].to_string();
        last_subbullet = Some(i);
        if let Some(colon) = trimmed.find(':') {
            if colon > 2 && classify_field(trimmed[2..colon].trim()) == Some(ClaimField::LastVerified)
            {
                last_verified_line = Some(i);
            }
        }
    }

    match last_verified_line {
        Some(i) => {
            // substitui o valor, preservando label e indentação originais
            let colon = lines[i].find(':').unwrap();
            lines[i] = format!("{} {date}", &lines[i][..=colon]);
        }
        None => {
            let insert_at = last_subbullet.map(|i| i + 1).unwrap_or(claim.text_line + 1);
            lines.insert(insert_at, format!("{indent}- Last verified: {date}"));
        }
    }

    let mut result = lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

// ── Parsing interno ──────────────────────────────────────────────────────────

/// Parseia os claims com a posição das linhas de cada um.
fn parse_claims_detailed(content: &str) -> Vec<ParsedClaim> {
    let lines: Vec<&str> = content.lines().collect();

    let Some(header) = lines.iter().position(|l| is_claims_header(l)) else {
        return Vec::new();
    };

    // A seção vai do cabeçalho até a próxima linha de heading (ou EOF).
    let section_end = (header + 1..lines.len())
        .find(|&i| lines[i].starts_with('#'))
        .unwrap_or(lines.len());

    // Linhas que iniciam um claim: bullets de nível superior (`- ` sem indentação).
    let claim_starts: Vec<usize> = (header + 1..section_end)
        .filter(|&i| lines[i].starts_with("- "))
        .collect();

    let mut result = Vec::new();
    for (idx, &start) in claim_starts.iter().enumerate() {
        let block_end = claim_starts.get(idx + 1).copied().unwrap_or(section_end);

        let mut claim = Claim {
            text: lines[start][2..].trim().to_string(),
            ..Claim::default()
        };

        for line in &lines[start + 1..block_end] {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("- ") {
                continue;
            }
            let Some(colon) = trimmed.find(':') else {
                continue;
            };
            if colon <= 2 {
                continue;
            }
            let label = trimmed[2..colon].trim();
            let value = trimmed[colon + 1..].trim().to_string();
            match classify_field(label) {
                Some(ClaimField::Source) => claim.source = Some(value),
                Some(ClaimField::Confidence) => claim.confidence = Some(value),
                Some(ClaimField::LastVerified) => claim.last_verified = Some(value),
                None => {}
            }
        }

        result.push(ParsedClaim {
            claim,
            text_line: start,
            block_end,
        });
    }
    result
}

/// verifica se uma linha é o cabeçalho da seção de claims.
fn is_claims_header(line: &str) -> bool {
    let line = line.trim_end();
    CLAIMS_HEADERS.iter().any(|h| line.eq_ignore_ascii_case(h))
}

/// classifica um label de campo (bilíngue EN/PT, case-insensitive).
fn classify_field(label: &str) -> Option<ClaimField> {
    match label.to_lowercase().as_str() {
        "source" | "fonte" => Some(ClaimField::Source),
        "confidence" | "confiança" | "confianca" => Some(ClaimField::Confidence),
        "last verified" | "última verificação" | "ultima verificacao" => {
            Some(ClaimField::LastVerified)
        }
        _ => None,
    }
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Página\n\nIntro.\n\n## Claims\n\n- A plataforma usa três escopos de vector store.\n  - Source: `wiki://page/decisao-vetores`\n  - Confidence: high\n  - Last verified: 2026-05-11\n\n- O serviço fala gRPC com a API.\n  - Source: `raw://source/sessao-grpc`\n  - Confidence: medium\n  - Last verified: 2026-05-10\n\n## Veja também\n\n- outra coisa";

    // ── parse_claims ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_claims_basic() {
        let claims = parse_claims(SAMPLE);
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].text, "A plataforma usa três escopos de vector store.");
        assert_eq!(claims[0].source.as_deref(), Some("`wiki://page/decisao-vetores`"));
        assert_eq!(claims[0].confidence.as_deref(), Some("high"));
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-11"));
    }

    #[test]
    fn test_parse_claims_stops_at_next_heading() {
        // o claim "outra coisa" está fora da seção (depois de `## Veja também`)
        let claims = parse_claims(SAMPLE);
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().all(|c| c.text != "outra coisa"));
    }

    #[test]
    fn test_parse_claims_none_when_no_section() {
        assert!(parse_claims("# Título\n\nSem claims aqui.").is_empty());
    }

    #[test]
    fn test_parse_claims_portuguese_labels() {
        let content = "## Claims\n\n- Afirmação em PT.\n  - Fonte: `wiki://page/x`\n  - Confiança: alta\n  - Última verificação: 2026-05-01";
        let claims = parse_claims(content);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].source.as_deref(), Some("`wiki://page/x`"));
        assert_eq!(claims[0].confidence.as_deref(), Some("alta"));
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-01"));
    }

    #[test]
    fn test_parse_claims_missing_fields() {
        let content = "## Claims\n\n- Claim sem metadados.\n\n- Claim só com source.\n  - Source: `raw://source/y`";
        let claims = parse_claims(content);
        assert_eq!(claims.len(), 2);
        assert!(claims[0].source.is_none());
        assert!(claims[0].confidence.is_none());
        assert!(claims[0].last_verified.is_none());
        assert_eq!(claims[1].source.as_deref(), Some("`raw://source/y`"));
        assert!(claims[1].last_verified.is_none());
    }

    #[test]
    fn test_parse_claims_header_at_eof() {
        let content = "## Claims\n\n- Único claim.\n  - Confidence: low";
        let claims = parse_claims(content);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].confidence.as_deref(), Some("low"));
    }

    #[test]
    fn test_parse_claims_empty_section() {
        let content = "## Claims\n\n## Outra Seção\n\n- não é claim";
        assert!(parse_claims(content).is_empty());
    }

    // ── set_last_verified ────────────────────────────────────────────────────

    #[test]
    fn test_set_last_verified_replaces_existing() {
        let result = set_last_verified(SAMPLE, 0, "2026-05-16").unwrap();
        let claims = parse_claims(&result);
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-16"));
        // o segundo claim não é tocado
        assert_eq!(claims[1].last_verified.as_deref(), Some("2026-05-10"));
    }

    #[test]
    fn test_set_last_verified_inserts_when_missing() {
        let content = "## Claims\n\n- Claim sem data.\n  - Source: `wiki://page/x`";
        let result = set_last_verified(content, 0, "2026-05-16").unwrap();
        let claims = parse_claims(&result);
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-16"));
        // o source original é preservado
        assert_eq!(claims[0].source.as_deref(), Some("`wiki://page/x`"));
    }

    #[test]
    fn test_set_last_verified_inserts_when_no_subbullets() {
        let content = "## Claims\n\n- Claim sem nenhum sub-bullet.";
        let result = set_last_verified(content, 0, "2026-05-16").unwrap();
        let claims = parse_claims(&result);
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-16"));
    }

    #[test]
    fn test_set_last_verified_preserves_portuguese_label() {
        let content = "## Claims\n\n- Afirmação.\n  - Última verificação: 2026-01-01";
        let result = set_last_verified(content, 0, "2026-05-16").unwrap();
        assert!(result.contains("Última verificação: 2026-05-16"));
        assert!(!result.contains("2026-01-01"));
    }

    #[test]
    fn test_set_last_verified_invalid_index() {
        let err = set_last_verified(SAMPLE, 9, "2026-05-16").unwrap_err();
        assert!(err.contains("inválido"));
    }

    #[test]
    fn test_set_last_verified_no_section() {
        let err = set_last_verified("# Sem claims", 0, "2026-05-16").unwrap_err();
        assert!(err.contains("Claims"));
    }

    #[test]
    fn test_set_last_verified_does_not_corrupt_other_content() {
        let result = set_last_verified(SAMPLE, 1, "2026-05-16").unwrap();
        assert!(result.contains("# Página"));
        assert!(result.contains("## Veja também"));
        assert!(result.contains("O serviço fala gRPC com a API."));
        let claims = parse_claims(&result);
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[1].last_verified.as_deref(), Some("2026-05-16"));
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    #[test]
    fn test_is_claims_header() {
        assert!(is_claims_header("## Claims"));
        assert!(is_claims_header("## Afirmações"));
        assert!(is_claims_header("## claims  "));
        assert!(!is_claims_header("## Claimsx"));
        assert!(!is_claims_header("### Claims"));
    }

    #[test]
    fn test_classify_field() {
        assert_eq!(classify_field("Source"), Some(ClaimField::Source));
        assert_eq!(classify_field("fonte"), Some(ClaimField::Source));
        assert_eq!(classify_field("Confiança"), Some(ClaimField::Confidence));
        assert_eq!(classify_field("Last verified"), Some(ClaimField::LastVerified));
        assert_eq!(classify_field("Última verificação"), Some(ClaimField::LastVerified));
        assert_eq!(classify_field("Outro"), None);
    }
}
