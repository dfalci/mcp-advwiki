// Resolução do diretório de trabalho da wiki.
//
// A wiki não vive mais dentro do projeto: cada caminho de origem (o `--root`
// informado, ou o diretório corrente) mapeia para um workspace próprio em
// `~/.advwiki/projects/<slug>/`, que preserva a mesma estrutura de sempre
// (`.advwiki/`, `.advwikilog.md`, `rawindex.md`). Assim o repositório do
// projeto fica limpo e o agente não tropeça nas páginas da wiki ao varrer
// arquivos.
//
// Wikis legadas — as que ainda estão dentro do projeto — são migradas uma
// única vez, no boot. A migração é `copia → verifica → rename → apaga origem`,
// com staging fora do destino final: se ela morrer no meio, o destino ainda
// não existe e a origem continua intacta, então a subida seguinte tenta de
// novo. Escrever direto no destino arriscaria deixá-lo pela metade e, como um
// destino existente faz a origem ser ignorada, isso seria perda silenciosa.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Nome do diretório, sob o home do usuário, que agrupa todos os workspaces.
const ADVWIKI_HOME_DIR: &str = ".advwiki";

/// Subdiretório onde cada projeto ganha o seu workspace.
const PROJECTS_SUBDIR: &str = "projects";

/// Itens do projeto que compõem uma wiki legada e são levados na migração.
const LEGACY_DIR: &str = ".advwiki";
const LEGACY_FILES: [&str; 2] = [".advwikilog.md", "rawindex.md"];

/// Acima deste tamanho o slug é truncado e desambiguado por hash — nome de
/// arquivo tem limite de 255 caracteres na maioria dos sistemas, e um projeto
/// aninhado fundo estouraria isso.
const MAX_SLUG_LEN: usize = 120;

/// Resolve o workspace da wiki para o caminho de origem informado, migrando
/// uma wiki legada se for o caso.
///
/// `root_arg` é o `--root` da linha de comando; quando ausente, a origem é o
/// diretório corrente. O retorno é o diretório que o `WikiFileManager` deve
/// usar como raiz.
pub fn resolve(root_arg: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let origin = match root_arg {
        Some(path) => path,
        None => std::env::current_dir().context("Falha ao obter o diretório corrente")?,
    };
    resolve_in(&advwiki_home()?, &origin)
}

/// Núcleo de [`resolve`], com o `~/.advwiki/` injetado — é o que os testes usam
/// para não depender das variáveis de ambiente do processo.
fn resolve_in(home: &Path, origin: &Path) -> anyhow::Result<PathBuf> {
    let home = normalize(home);
    let origin = normalize(origin);

    // Origem já dentro do próprio `~/.advwiki/` (alguém apontou `--root` para um
    // workspace): usa como está, em vez de criar um workspace de um workspace.
    if origin.starts_with(&home) {
        return Ok(origin);
    }

    let dest = home.join(PROJECTS_SUBDIR).join(slug_for(&origin));

    if dest.exists() {
        if has_legacy_wiki(&origin) {
            tracing::warn!(
                origem = %origin.display(),
                workspace = %dest.display(),
                "Workspace centralizado já existe — o conteúdo de `.advwiki/` no projeto será ignorado"
            );
        }
        return Ok(dest);
    }

    if has_legacy_wiki(&origin) {
        migrate(&origin, &dest)?;
    } else {
        fs::create_dir_all(&dest)
            .with_context(|| format!("Falha ao criar o workspace: {}", dest.display()))?;
    }

    Ok(dest)
}

/// `~/.advwiki/`, a partir de `USERPROFILE` (Windows) ou `HOME` (Unix).
fn advwiki_home() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("Nem USERPROFILE nem HOME estão definidos — impossível localizar o home do usuário")?;
    Ok(PathBuf::from(home).join(ADVWIKI_HOME_DIR))
}

/// Absolutiza e canonicaliza o caminho, removendo o prefixo `\\?\` que o
/// Windows devolve. Caminho inexistente não pode ser canonicalizado, então
/// cai no absoluto simples.
fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    match fs::canonicalize(&absolute) {
        Ok(canonical) => {
            let text = canonical.to_string_lossy();
            match text.strip_prefix(r"\\?\") {
                Some(stripped) => PathBuf::from(stripped),
                None => canonical,
            }
        }
        Err(_) => absolute,
    }
}

/// Slug determinístico do caminho de origem: todo caractere não-alfanumérico
/// vira `-`. `C:\teste` → `C--teste`.
///
/// A canonicalização feita em [`normalize`] é o que garante estabilidade — no
/// Windows ela devolve o caixa real do sistema de arquivos, então `c:\teste` e
/// `C:\Teste` chegam aqui idênticos.
pub fn slug_for(origin: &Path) -> String {
    let raw: String = origin
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    if raw.is_empty() {
        return "root".to_string();
    }

    if raw.chars().count() <= MAX_SLUG_LEN {
        return raw;
    }

    // Trunca preservando o começo (legível) e desambigua pelo hash do caminho
    // completo, senão dois projetos aninhados fundo sob a mesma árvore colidiriam.
    use md5::Digest;
    let hash = format!("{:x}", md5::Md5::digest(origin.to_string_lossy().as_bytes()));
    let head: String = raw.chars().take(MAX_SLUG_LEN - hash.len() - 1).collect();
    format!("{head}-{hash}")
}

/// `true` se o projeto ainda guarda uma wiki no formato antigo.
fn has_legacy_wiki(origin: &Path) -> bool {
    origin.join(LEGACY_DIR).is_dir()
        || LEGACY_FILES
            .iter()
            .any(|name| origin.join(name).is_file())
}

/// Move a wiki legada de `origin` para `dest`.
///
/// Qualquer falha antes do `rename` aborta com a origem intacta — o chamador
/// derruba o boot, o que é preferível a rodar com wiki incompleta. Falha ao
/// apagar a origem, depois do `rename`, é só um aviso: os dados já estão
/// íntegros no destino, e o resto local passa a ser ignorado de qualquer forma.
fn migrate(origin: &Path, dest: &Path) -> anyhow::Result<()> {
    let projects_dir = dest
        .parent()
        .context("Destino do workspace não tem diretório pai")?;
    let slug = dest
        .file_name()
        .context("Destino do workspace não tem nome de diretório")?
        .to_string_lossy()
        .to_string();
    let staging = projects_dir.join(format!(".tmp-{slug}"));

    // Sobra de uma tentativa anterior interrompida: descartada sem dó, já que a
    // origem só é apagada depois do rename.
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| {
            format!("Falha ao limpar staging de migração anterior: {}", staging.display())
        })?;
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("Falha ao criar staging de migração: {}", staging.display()))?;

    if let Err(e) = copy_into_staging(origin, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    fs::rename(&staging, dest)
        .inspect_err(|_| {
            let _ = fs::remove_dir_all(&staging);
        })
        .with_context(|| {
            format!(
                "Falha ao promover o staging para o workspace: {} → {}",
                staging.display(),
                dest.display()
            )
        })?;

    tracing::info!(
        origem = %origin.display(),
        workspace = %dest.display(),
        "Wiki migrada para o workspace centralizado"
    );

    remove_legacy(origin);
    Ok(())
}

/// Copia `.advwiki/`, `.advwikilog.md` e `rawindex.md` para o staging e confere
/// que o que chegou lá bate com a origem.
fn copy_into_staging(origin: &Path, staging: &Path) -> anyhow::Result<()> {
    let source_dir = origin.join(LEGACY_DIR);
    if source_dir.is_dir() {
        let target_dir = staging.join(LEGACY_DIR);
        copy_dir_all(&source_dir, &target_dir).with_context(|| {
            format!(
                "Falha ao copiar {} para {}",
                source_dir.display(),
                target_dir.display()
            )
        })?;

        let (source_files, source_bytes) = count_and_size(&source_dir)?;
        let (target_files, target_bytes) = count_and_size(&target_dir)?;
        if source_files != target_files || source_bytes != target_bytes {
            bail!(
                "Migração incompleta de {}: origem tem {source_files} arquivos/{source_bytes} bytes, \
                 cópia tem {target_files} arquivos/{target_bytes} bytes",
                source_dir.display()
            );
        }
    }

    for name in LEGACY_FILES {
        let source = origin.join(name);
        if !source.is_file() {
            continue;
        }
        let target = staging.join(name);
        fs::copy(&source, &target)
            .with_context(|| format!("Falha ao copiar {}", source.display()))?;

        let source_len = fs::metadata(&source)?.len();
        let target_len = fs::metadata(&target)?.len();
        if source_len != target_len {
            bail!(
                "Migração incompleta de {}: origem tem {source_len} bytes, cópia tem {target_len}",
                source.display()
            );
        }
    }

    Ok(())
}

/// Apaga os itens da wiki legada. Best-effort: o dado já está seguro no
/// destino, então uma falha aqui (arquivo travado por outro processo, por
/// exemplo) não justifica derrubar o servidor.
fn remove_legacy(origin: &Path) {
    let dir = origin.join(LEGACY_DIR);
    if dir.is_dir()
        && let Err(e) = fs::remove_dir_all(&dir)
    {
        tracing::warn!(
            error = %e,
            path = %dir.display(),
            "Wiki migrada, mas o diretório antigo não pôde ser removido — apague-o manualmente"
        );
    }

    for name in LEGACY_FILES {
        let file = origin.join(name);
        if file.is_file()
            && let Err(e) = fs::remove_file(&file)
        {
            tracing::warn!(
                error = %e,
                path = %file.display(),
                "Wiki migrada, mas o arquivo antigo não pôde ser removido — apague-o manualmente"
            );
        }
    }
}

/// Cópia recursiva de diretório.
fn copy_dir_all(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Conta arquivos e soma bytes de uma árvore — a verificação que autoriza o
/// `rename` e, com ele, o descarte da origem.
fn count_and_size(dir: &Path) -> anyhow::Result<(usize, u64)> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry
            .with_context(|| format!("Falha ao percorrer {}", dir.display()))?;
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry.metadata()?.len();
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Cria uma wiki legada em `root` com uma página, log e rawindex.
    fn make_legacy_wiki(root: &Path) {
        let pages = root.join(".advwiki").join("pages");
        fs::create_dir_all(&pages).unwrap();
        fs::write(pages.join("alpha.md"), "# Alpha\n").unwrap();
        fs::create_dir_all(root.join(".advwiki").join("index")).unwrap();
        fs::write(root.join(".advwiki").join(".schema-version"), "2").unwrap();
        fs::write(root.join(".advwikilog.md"), "# AdvWiki Log\n").unwrap();
        fs::write(root.join("rawindex.md"), "# Índice\n").unwrap();
    }

    #[test]
    fn slug_troca_separadores_por_hifen() {
        assert_eq!(slug_for(Path::new(r"C:\teste")), "C--teste");
        assert_eq!(
            slug_for(Path::new(r"C:\desenvolvimento\dcoder\mcp-advwiki")),
            "C--desenvolvimento-dcoder-mcp-advwiki"
        );
    }

    #[test]
    fn slug_de_caminho_longo_cabe_no_limite() {
        let longo = PathBuf::from(format!(r"C:\{}", "a".repeat(400)));
        let slug = slug_for(&longo);
        assert_eq!(slug.chars().count(), MAX_SLUG_LEN);
        // Caminhos longos distintos não podem colidir só porque o prefixo bate.
        let outro = PathBuf::from(format!(r"C:\{}b", "a".repeat(400)));
        assert_ne!(slug, slug_for(&outro));
    }

    #[test]
    fn caminhos_equivalentes_geram_o_mesmo_workspace() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();
        let subdir = projeto.path().join("sub");
        fs::create_dir_all(&subdir).unwrap();

        let direto = resolve_in(home.path(), &subdir).unwrap();
        let com_ponto = resolve_in(home.path(), &projeto.path().join("sub").join(".")).unwrap();

        assert_eq!(direto, com_ponto);
    }

    #[test]
    fn projeto_sem_wiki_ganha_workspace_vazio() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();

        let workspace = resolve_in(home.path(), projeto.path()).unwrap();

        assert!(workspace.is_dir());
        assert!(workspace.starts_with(home.path().join(PROJECTS_SUBDIR)));
        assert!(!workspace.join(".advwiki").exists());
    }

    #[test]
    fn wiki_legada_e_movida_para_o_workspace() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();
        make_legacy_wiki(projeto.path());

        let workspace = resolve_in(home.path(), projeto.path()).unwrap();

        // Chegou tudo no destino…
        assert_eq!(
            fs::read_to_string(workspace.join(".advwiki/pages/alpha.md")).unwrap(),
            "# Alpha\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".advwiki/.schema-version")).unwrap(),
            "2"
        );
        assert!(workspace.join(".advwikilog.md").is_file());
        assert!(workspace.join("rawindex.md").is_file());

        // …e a origem ficou limpa.
        assert!(!projeto.path().join(".advwiki").exists());
        assert!(!projeto.path().join(".advwikilog.md").exists());
        assert!(!projeto.path().join("rawindex.md").exists());

        // Sem staging órfão.
        let sobras: Vec<_> = fs::read_dir(home.path().join(PROJECTS_SUBDIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".tmp-"))
            .collect();
        assert!(sobras.is_empty(), "staging não foi limpo: {sobras:?}");
    }

    #[test]
    fn migracao_e_feita_uma_unica_vez() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();
        make_legacy_wiki(projeto.path());

        let workspace = resolve_in(home.path(), projeto.path()).unwrap();
        fs::write(workspace.join(".advwiki/pages/alpha.md"), "# Editada\n").unwrap();

        // Wiki legada reaparece no projeto (recriada por engano, resto de backup…).
        make_legacy_wiki(projeto.path());
        let de_novo = resolve_in(home.path(), projeto.path()).unwrap();

        assert_eq!(de_novo, workspace);
        // O workspace mandou: nada foi sobrescrito pelo conteúdo local…
        assert_eq!(
            fs::read_to_string(workspace.join(".advwiki/pages/alpha.md")).unwrap(),
            "# Editada\n"
        );
        // …e o local foi deixado intocado.
        assert!(projeto.path().join(".advwiki/pages/alpha.md").is_file());
    }

    #[test]
    fn falha_na_copia_preserva_a_origem_e_nao_cria_o_workspace() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();
        make_legacy_wiki(projeto.path());

        // `projects/` ocupado por um arquivo: `create_dir_all` do staging falha.
        fs::write(home.path().join(PROJECTS_SUBDIR), "bloqueio").unwrap();

        let erro = resolve_in(home.path(), projeto.path());

        assert!(erro.is_err(), "migração deveria ter falhado");
        assert!(projeto.path().join(".advwiki/pages/alpha.md").is_file());
        assert!(projeto.path().join(".advwikilog.md").is_file());
        assert!(projeto.path().join("rawindex.md").is_file());
    }

    #[test]
    fn staging_orfao_de_tentativa_anterior_e_descartado() {
        let home = TempDir::new().unwrap();
        let projeto = TempDir::new().unwrap();
        make_legacy_wiki(projeto.path());

        let projects = home.path().join(PROJECTS_SUBDIR);
        let slug = slug_for(&normalize(projeto.path()));
        let staging = projects.join(format!(".tmp-{slug}"));
        fs::create_dir_all(staging.join(".advwiki/pages")).unwrap();
        fs::write(staging.join(".advwiki/pages/lixo.md"), "resto").unwrap();

        let workspace = resolve_in(home.path(), projeto.path()).unwrap();

        assert!(!workspace.join(".advwiki/pages/lixo.md").exists());
        assert!(workspace.join(".advwiki/pages/alpha.md").is_file());
        assert!(!staging.exists());
    }

    #[test]
    fn origem_dentro_do_advwiki_home_e_usada_como_esta() {
        let home = TempDir::new().unwrap();
        let dentro = home.path().join(PROJECTS_SUBDIR).join("c--teste");
        fs::create_dir_all(&dentro).unwrap();

        let workspace = resolve_in(home.path(), &dentro).unwrap();

        assert_eq!(workspace, normalize(&dentro));
    }
}
