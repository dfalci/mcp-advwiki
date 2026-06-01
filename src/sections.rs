// ── Módulo de Edição por Seção ───────────────────────────────────────────────
//
// Permite substituir (`Replace`) ou anexar (`Append`) conteúdo a UMA seção de
// uma página markdown — delimitada por um heading ATX (`#`..`######`) — sem
// reenviar o documento inteiro. Usado por `update_page` e `propose_page_update`
// quando o argumento `section` é informado.
//
// Mover a reconstrução do documento da IA (propensa a esquecer trechos num
// `overwrite`) para o servidor (determinístico) é o ponto: a IA manda só o
// pedaço da seção; o resto da página é preservado byte a byte.
//
// Garantias de robustez (cada uma é uma lição do resto da base):
//   - Delimita a seção por NÍVEL de heading: `## X` termina no próximo heading
//     de nível <= 2; subseções `### Y` continuam dentro. (`claims.rs` corta em
//     QUALQUER `#` — correto só porque claims não têm subseções.)
//   - Ignora headings dentro de blocos de código cercados (``` ``` ``` / `~~~`),
//     como `migration.rs` faz — senão um `# comentário` dentro de um code block
//     fecharia a seção no lugar errado.
//   - Preserva o terminador de linha (CRLF vs LF) via `split_inclusive`, em vez
//     de normalizar para LF (o que `set_last_verified` faz e geraria um diff
//     espúrio gigante em arquivos salvos no Windows/Obsidian).
//   - Preserva o frontmatter YAML intacto (não procura headings dentro dele).

/// Modo de edição de uma seção.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionMode {
    /// Substitui o corpo da seção pelo novo conteúdo.
    Replace,
    /// Adiciona o novo conteúdo ao fim do corpo da seção, preservando o atual.
    Append,
}

/// Um heading localizado no corpo da página: posição da linha, nível e título.
struct Heading {
    /// índice (0-based) da linha do heading no vetor de linhas.
    line: usize,
    /// nível ATX (1 a 6 — número de `#`).
    level: usize,
    /// título já sem os `#` e espaços, sem fechamento ATX opcional.
    title: String,
}

/// Lista as seções (headings ATX) de uma página, na ordem em que aparecem.
/// Retorna pares `(nível, título)`. Headings dentro de code blocks ou do
/// frontmatter são ignorados.
pub fn list_sections(content: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    scan_headings(&lines)
        .into_iter()
        .map(|h| (h.level, h.title))
        .collect()
}

/// Substitui ou anexa o corpo da seção cujo título casa com `heading`.
///
/// `heading` pode vir com ou sem os `#` (`"Details"` ou `"## Details"`); a
/// comparação é case-insensitive e ignora espaços nas pontas.
///
/// Erros (todos preservam o conteúdo original — nada é gravado):
///   - a seção não existe (a mensagem lista as seções disponíveis);
///   - o título é ambíguo (mais de uma seção com o mesmo nome).
pub fn edit_section(
    content: &str,
    heading: &str,
    new_body: &str,
    mode: SectionMode,
) -> Result<String, String> {
    let target = heading.trim().trim_start_matches('#').trim();
    if target.is_empty() {
        return Err("nome de seção vazio".to_string());
    }
    let target_lc = target.to_lowercase();

    // Preserva o terminador de linha dominante do arquivo (CRLF em Windows).
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };

    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let headings = scan_headings(&lines);

    let matched: Vec<&Heading> = headings
        .iter()
        .filter(|h| h.title.to_lowercase() == target_lc)
        .collect();

    if matched.is_empty() {
        let secs = list_sections(content);
        let available = if secs.is_empty() {
            "a página não tem nenhuma seção".to_string()
        } else {
            format!(
                "seções disponíveis: {}",
                secs.iter()
                    .map(|(_, title)| format!("'{title}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(format!("seção '{target}' não encontrada ({available})"));
    }
    if matched.len() > 1 {
        return Err(format!(
            "seção '{target}' é ambígua: {} ocorrências na página. \
             Renomeie uma delas ou edite a página inteira.",
            matched.len()
        ));
    }

    let h = matched[0];
    // Fim da seção: o primeiro heading posterior com nível <= ao do alvo.
    // Subseções (nível maior) ficam dentro do corpo e são incluídas.
    let end = headings
        .iter()
        .filter(|o| o.line > h.line && o.level <= h.level)
        .map(|o| o.line)
        .min()
        .unwrap_or(lines.len());

    let body_norm = normalize_body(new_body, eol);
    let combined = match mode {
        SectionMode::Replace => body_norm,
        SectionMode::Append => {
            let existing = normalize_body(&lines[h.line + 1..end].concat(), eol);
            join_blocks(&existing, &body_norm, eol)
        }
    };

    let mut out = String::with_capacity(content.len() + new_body.len());
    // Tudo até e incluindo a linha do heading, intacto.
    out.push_str(&lines[..=h.line].concat());
    if !out.ends_with('\n') {
        out.push_str(eol);
    }
    // Linha em branco após o heading.
    out.push_str(eol);
    if !combined.is_empty() {
        out.push_str(&combined);
        out.push_str(eol);
    }
    // Linha em branco separadora antes da próxima seção, se houver.
    if end < lines.len() {
        out.push_str(eol);
    }
    // Da próxima seção em diante, intacto.
    out.push_str(&lines[end..].concat());

    Ok(out)
}

// ── Parsing interno ──────────────────────────────────────────────────────────

/// Varre as linhas e retorna os headings ATX fora do frontmatter e de code
/// blocks. O frontmatter (bloco inicial entre `---`) é pulado; blocos cercados
/// por ``` ``` ``` ou `~~~` são ignorados (incluindo `#` dentro deles).
fn scan_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();

    // Pula o frontmatter inicial, se houver (`---` ... `---`).
    let mut start = 0;
    if lines.first().map(|l| l.trim_end()) == Some("---")
        && let Some(close) = (1..lines.len()).find(|&i| lines[i].trim_end() == "---")
    {
        start = close + 1;
    }

    let mut fence: Option<char> = None;
    for (i, line) in lines.iter().enumerate().skip(start) {
        if let Some(fc) = fence_char(line) {
            match fence {
                None => fence = Some(fc),
                Some(open) if open == fc => fence = None,
                Some(_) => {} // marcador de tipo diferente dentro do fence — ignora
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some((level, title)) = parse_heading(line) {
            headings.push(Heading {
                line: i,
                level,
                title,
            });
        }
    }
    headings
}

/// Reconhece uma linha de heading ATX. Retorna `(nível, título)`.
///
/// Regras CommonMark relevantes: 1 a 6 `#`, seguidos de espaço/tab (ou fim de
/// linha); indentação de 4+ espaços é bloco de código, não heading; sequência
/// de `#` no fim é fechamento opcional (`## Título ##`).
fn parse_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent >= 4 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &trimmed[hashes..];
    let rest = after.trim_end();
    // Precisa de espaço/tab após os `#` — a menos que seja um heading vazio.
    if !rest.is_empty() && !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let title = rest.trim_end_matches('#').trim().to_string();
    Some((hashes, title))
}

/// Detecta a abertura/fechamento de um code block cercado. Retorna o caractere
/// marcador (`` ` `` ou `~`) se a linha começa com 3+ deles.
fn fence_char(line: &str) -> Option<char> {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent >= 4 {
        return None;
    }
    if trimmed.starts_with("```") {
        Some('`')
    } else if trimmed.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

/// Remove linhas em branco das pontas de um bloco e normaliza o terminador de
/// linha interno para `eol`. Retorna string vazia se o bloco só tem espaços.
fn normalize_body(body: &str, eol: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let first = lines.iter().position(|l| !l.trim().is_empty());
    let Some(first) = first else {
        return String::new();
    };
    let last = lines.iter().rposition(|l| !l.trim().is_empty()).unwrap();
    lines[first..=last].join(eol)
}

/// Junta dois blocos já normalizados com uma linha em branco entre eles,
/// ignorando os que estiverem vazios.
fn join_blocks(a: &str, b: &str, eol: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        (false, false) => format!("{a}{eol}{eol}{b}"),
    }
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "---\ntype: note\n---\n\n# Título\n\nIntro.\n\n## Detalhes\n\nCorpo antigo.\n\n## Veja também\n\n- [[outra]]\n";

    // ── parse_heading ────────────────────────────────────────────────────────

    #[test]
    fn test_parse_heading_levels() {
        assert_eq!(parse_heading("# A\n"), Some((1, "A".into())));
        assert_eq!(parse_heading("### Três\n"), Some((3, "Três".into())));
        assert_eq!(parse_heading("###### Seis\n"), Some((6, "Seis".into())));
    }

    #[test]
    fn test_parse_heading_rejects_non_headings() {
        assert_eq!(parse_heading("#hashtag\n"), None); // sem espaço após #
        assert_eq!(parse_heading("####### Sete\n"), None); // 7 # > 6
        assert_eq!(parse_heading("    # Indentado demais\n"), None); // 4 espaços = code
        assert_eq!(parse_heading("texto comum\n"), None);
    }

    #[test]
    fn test_parse_heading_strips_atx_closing() {
        assert_eq!(parse_heading("## Título ##\n"), Some((2, "Título".into())));
    }

    // ── scan_headings ────────────────────────────────────────────────────────

    #[test]
    fn test_scan_skips_frontmatter_and_fences() {
        let content = "---\ntype: note\n# isto não é heading (frontmatter)\n---\n\n## Real\n\n```\n# comentário em code block\n```\n\n## Outra\n";
        let secs = list_sections(content);
        assert_eq!(secs, vec![(2, "Real".into()), (2, "Outra".into())]);
    }

    // ── edit_section: replace ────────────────────────────────────────────────

    #[test]
    fn test_replace_middle_section() {
        let out = edit_section(PAGE, "Detalhes", "Corpo NOVO.", SectionMode::Replace).unwrap();
        assert!(out.contains("Corpo NOVO."));
        assert!(!out.contains("Corpo antigo."));
        // não toca nas outras seções
        assert!(out.contains("# Título"));
        assert!(out.contains("Intro."));
        assert!(out.contains("## Veja também"));
        assert!(out.contains("- [[outra]]"));
        // frontmatter preservado
        assert!(out.starts_with("---\ntype: note\n---\n"));
    }

    #[test]
    fn test_replace_last_section_eof() {
        let out = edit_section(PAGE, "Veja também", "- [[nova-ref]]", SectionMode::Replace).unwrap();
        assert!(out.contains("- [[nova-ref]]"));
        assert!(!out.contains("- [[outra]]"));
        assert!(out.contains("Corpo antigo.")); // detalhes intactos
    }

    #[test]
    fn test_replace_accepts_heading_with_hashes_and_case() {
        let out = edit_section(PAGE, "## detalhes", "X.", SectionMode::Replace).unwrap();
        assert!(out.contains("X."));
        assert!(!out.contains("Corpo antigo."));
    }

    // ── edit_section: append ─────────────────────────────────────────────────

    #[test]
    fn test_append_preserves_existing_body() {
        let out = edit_section(PAGE, "Detalhes", "Linha extra.", SectionMode::Append).unwrap();
        assert!(out.contains("Corpo antigo."));
        assert!(out.contains("Linha extra."));
        // a ordem: antigo antes do novo
        let old = out.find("Corpo antigo.").unwrap();
        let new = out.find("Linha extra.").unwrap();
        assert!(old < new);
    }

    // ── subseções (delimitação por nível) ────────────────────────────────────

    #[test]
    fn test_replace_section_includes_its_subsections() {
        let content = "## Pai\n\ntexto pai\n\n### Filho\n\ntexto filho\n\n## Tio\n\ntexto tio\n";
        let out = edit_section(content, "Pai", "novo conteúdo do pai", SectionMode::Replace).unwrap();
        // a subseção Filho fazia parte de Pai → some
        assert!(!out.contains("### Filho"));
        assert!(!out.contains("texto filho"));
        assert!(out.contains("novo conteúdo do pai"));
        // Tio (mesmo nível) é preservado
        assert!(out.contains("## Tio"));
        assert!(out.contains("texto tio"));
    }

    #[test]
    fn test_replace_subsection_does_not_touch_parent_or_sibling() {
        let content = "## Pai\n\ntexto pai\n\n### Filho\n\ntexto filho\n\n### Filho2\n\ntexto filho2\n";
        let out = edit_section(content, "Filho", "FILHO EDITADO", SectionMode::Replace).unwrap();
        assert!(out.contains("texto pai"));
        assert!(out.contains("FILHO EDITADO"));
        assert!(!out.contains("texto filho\n")); // o antigo corpo do Filho some
        assert!(out.contains("### Filho2"));
        assert!(out.contains("texto filho2"));
    }

    // ── code fence ───────────────────────────────────────────────────────────

    #[test]
    fn test_section_end_not_cut_by_hash_in_code_block() {
        let content = "## Exemplo\n\n```bash\n# isto é um comentário shell\necho oi\n```\n\n## Depois\n\nfim\n";
        let out = edit_section(content, "Exemplo", "```bash\n# novo comentário\n```", SectionMode::Replace).unwrap();
        // a seção "Depois" tem que sobreviver intacta
        assert!(out.contains("## Depois"));
        assert!(out.contains("fim"));
        assert!(out.contains("# novo comentário"));
        assert!(!out.contains("echo oi"));
    }

    // ── CRLF ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_crlf_is_preserved() {
        let content = "# Título\r\n\r\n## Alvo\r\n\r\nantigo\r\n\r\n## Fim\r\n\r\nz\r\n";
        let out = edit_section(content, "Alvo", "novo", SectionMode::Replace).unwrap();
        assert!(out.contains("\r\n"), "CRLF deve ser preservado");
        assert!(!out.contains("\n\n\n")); // sem LF cru introduzido
        assert!(out.contains("novo"));
        assert!(out.contains("## Fim\r\n"));
    }

    // ── erros ────────────────────────────────────────────────────────────────

    #[test]
    fn test_section_not_found_lists_available() {
        let err = edit_section(PAGE, "Inexistente", "x", SectionMode::Replace).unwrap_err();
        assert!(err.contains("não encontrada"));
        assert!(err.contains("Detalhes")); // lista as seções existentes
    }

    #[test]
    fn test_ambiguous_section_errors() {
        let content = "## Notas\n\na\n\n## Notas\n\nb\n";
        let err = edit_section(content, "Notas", "x", SectionMode::Replace).unwrap_err();
        assert!(err.contains("ambígua"));
    }

    #[test]
    fn test_empty_heading_arg_errors() {
        let err = edit_section(PAGE, "##", "x", SectionMode::Replace).unwrap_err();
        assert!(err.contains("vazio"));
    }

    // ── re-parse estável ─────────────────────────────────────────────────────

    #[test]
    fn test_result_is_reparseable() {
        let out = edit_section(PAGE, "Detalhes", "novo corpo", SectionMode::Replace).unwrap();
        // editar de novo a mesma seção continua funcionando sobre o resultado
        let out2 = edit_section(&out, "Detalhes", "outro corpo", SectionMode::Replace).unwrap();
        assert!(out2.contains("outro corpo"));
        assert!(!out2.contains("novo corpo"));
        // todas as seções continuam detectáveis
        let secs = list_sections(&out2);
        assert_eq!(
            secs,
            vec![(1, "Título".into()), (2, "Detalhes".into()), (2, "Veja também".into())]
        );
    }

    // ── Regressão: não quebrar páginas existentes ────────────────────────────

    /// Offset de byte do início da linha `idx` no vetor de linhas.
    fn line_byte_offset(lines: &[&str], idx: usize) -> usize {
        lines[..idx].iter().map(|l| l.len()).sum()
    }

    #[test]
    fn test_preserves_prefix_and_suffix_byte_for_byte() {
        let content = "---\ntype: note\n---\n\n# A\n\ncorpo a\n\n## B\n\ncorpo b\n\n### B1\n\nsub b1\n\n## C\n\ncorpo c\n";
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let headings = scan_headings(&lines);
        let b = headings.iter().find(|h| h.title == "B").unwrap();
        let prefix = line_byte_offset(&lines, b.line);

        let out = edit_section(content, "B", "NOVO B", SectionMode::Replace).unwrap();

        // Tudo acima do heading da seção editada é idêntico byte a byte:
        // frontmatter, a seção A inteira e seu corpo.
        assert_eq!(&out[..prefix], &content[..prefix], "prefixo foi alterado");
        // Tudo da próxima seção de mesmo nível em diante é idêntico.
        let c_out = out.find("## C").unwrap();
        let c_orig = content.find("## C").unwrap();
        assert_eq!(&out[c_out..], &content[c_orig..], "sufixo foi alterado");
        // a subseção B1 fazia parte de B → foi substituída
        assert!(!out.contains("### B1"));
        assert!(out.contains("NOVO B"));
    }

    /// Varre as páginas REAIS versionadas no repo (`.advwiki/pages`) e garante
    /// que editar a última seção de cada uma preserva o resto da página
    /// byte a byte. É a prova contra "quebrar páginas já existentes". Se o
    /// checkout não tiver as páginas, o teste sai sem falhar.
    #[test]
    fn test_regression_real_repo_pages_preserve_everything_above() {
        let pages_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".advwiki")
            .join("pages");
        if !pages_dir.exists() {
            return;
        }

        let mut edited = 0usize;
        for entry in std::fs::read_dir(&pages_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = content.split_inclusive('\n').collect();
            let headings = scan_headings(&lines);

            // Última seção de título único (evita o caso ambíguo, que por design
            // retorna erro em vez de editar).
            let target = headings.iter().rev().find(|h| {
                headings
                    .iter()
                    .filter(|o| o.title.to_lowercase() == h.title.to_lowercase())
                    .count()
                    == 1
            });
            let Some(h) = target else { continue };
            let prefix = line_byte_offset(&lines, h.line);

            let out = edit_section(&content, &h.title, "PLACEHOLDER DE REGRESSAO", SectionMode::Replace)
                .unwrap_or_else(|e| panic!("falha ao editar '{}' em {:?}: {e}", h.title, path));

            assert_eq!(
                &out[..prefix],
                &content[..prefix],
                "editar a seção '{}' alterou conteúdo acima dela em {:?}",
                h.title,
                path
            );
            assert!(out.contains("PLACEHOLDER DE REGRESSAO"));
            if content.starts_with("---") {
                assert!(out.starts_with("---"), "frontmatter perdido em {:?}", path);
            }
            edited += 1;
        }

        assert!(edited >= 1, "esperava editar ao menos uma seção de página real");
    }

    #[test]
    fn test_claims_survive_editing_other_section() {
        let content = "# Doc\n\n## Visão\n\ntexto\n\n## Claims\n\n- Afirmação X.\n  - Source: [[fonte]]\n  - Confidence: high\n  - Last verified: 2026-05-11\n";
        let out = edit_section(content, "Visão", "novo texto da visão", SectionMode::Replace).unwrap();
        // o bloco de claims, em OUTRA seção, continua íntegro e parseável
        let claims = crate::claims::parse_claims(&out);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].source.as_deref(), Some("[[fonte]]"));
        assert_eq!(claims[0].confidence.as_deref(), Some("high"));
        assert_eq!(claims[0].last_verified.as_deref(), Some("2026-05-11"));
        assert!(out.contains("novo texto da visão"));
    }

    #[test]
    fn test_setext_heading_not_a_section_and_not_corrupted() {
        // Setext (título sublinhado) NÃO é reconhecido como seção — só ATX.
        let content = "Título Setext\n=============\n\ntexto setext\n\n## ATX\n\ncorpo atx\n";
        // editar a seção ATX preserva o bloco setext acima intacto
        let out = edit_section(content, "ATX", "novo corpo", SectionMode::Replace).unwrap();
        assert!(out.contains("Título Setext\n============="));
        assert!(out.contains("texto setext"));
        assert!(out.contains("novo corpo"));
        // tentar editar a "seção" setext falha (não é heading ATX) — sem corromper
        let err = edit_section(content, "Título Setext", "x", SectionMode::Replace).unwrap_err();
        assert!(err.contains("não encontrada"));
    }

    #[test]
    fn test_new_body_preserves_internal_structure() {
        let content = "# T\n\n## S\n\nx\n";
        let body = "parágrafo 1\n\nparágrafo 2\n\n- item a\n- item b";
        let out = edit_section(content, "S", body, SectionMode::Replace).unwrap();
        assert!(out.contains("parágrafo 1\n\nparágrafo 2"));
        assert!(out.contains("- item a\n- item b"));
    }

    #[test]
    fn test_replace_and_append_on_empty_section() {
        let content = "# T\n\n## Vazia\n\n## Próxima\n\nz\n";
        let out = edit_section(content, "Vazia", "agora tem conteúdo", SectionMode::Replace).unwrap();
        assert!(out.contains("agora tem conteúdo"));
        assert!(out.contains("## Próxima"));
        assert!(out.contains("z"));

        let out2 = edit_section(content, "Vazia", "apenso", SectionMode::Append).unwrap();
        assert!(out2.contains("apenso"));
        assert!(out2.contains("## Próxima"));
    }
}
