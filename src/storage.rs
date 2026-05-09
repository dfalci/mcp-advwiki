// ── Módulo de Gerenciamento de Arquivos e Recursos ──────────────────────────
//
// Gerencia toda a I/O física da Wiki local dentro do diretório `.advwiki`.
// Mapeia URIs lógicas (ex: `wiki://page/home`) para caminhos seguros no disco,
// com proteção contra path traversal.
//
// Estrutura de diretórios gerenciada:
//
//   {root}/
//     .advwikilog.md          ← log operacional
//     rawindex.md             ← índice de raw sources
//     .advwiki/               ← diretório base da wiki
//       pages/                ← páginas em markdown
//         {slug}.md
//       sources/              ← conteúdo bruto extraído
//         {source_id}
//       metadata/             ← metadados de raw sources
//         {source_id}.json

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Metadados de uma raw source (arquivo externo indexado).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSourceMetadata {
    /// Identificador único da source (ex: hash do conteúdo).
    pub source_id: String,
    /// Caminho original do arquivo no sistema de onde foi extraído.
    pub original_path: Option<String>,
    /// Tamanho em bytes do conteúdo bruto.
    pub size_bytes: u64,
    /// MIME type detectado (ex: "text/markdown", "application/pdf").
    pub mime_type: Option<String>,
    /// Timestamp ISO 8601 da extração.
    pub extracted_at: String,
    /// Tags ou labels associadas (opcional).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Timestamp ISO 8601 da última modificação.
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl RawSourceMetadata {
    /// Cria um novo RawSourceMetadata com `extracted_at` preenchido como agora.
    pub fn new(source_id: String, size_bytes: u64) -> Self {
        Self {
            source_id,
            original_path: None,
            size_bytes,
            mime_type: None,
            extracted_at: chrono::Utc::now().to_rfc3339(),
            tags: Vec::new(),
            updated_at: None,
        }
    }
}

/// Entrada do rawindex.md. Cada linha representa uma raw source disponível.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawIndexEntry {
    pub source_id: String,
    pub original_path: Option<String>,
    pub extracted_at: String,
}

/// Gerente de arquivos da Wiki. Toda operação de I/O passa por esta struct.
#[derive(Debug, Clone)]
pub struct WikiFileManager {
    /// Diretório raiz do projeto (onde `.advwiki/` e `.advwikilog.md` residem).
    root: PathBuf,
    /// Subdiretório `.advwiki/` — todas as páginas e sources vivem aqui.
    wiki_dir: PathBuf,
}


/// URI lógicas suportadas pelo sistema (formato RFC: `esquema://path`).
///
/// | Esquema   | Formato                              | Descrição                    |
/// |-----------|--------------------------------------|------------------------------|
/// | `wiki://` | `wiki://log`                         | Log operacional              |
/// | `wiki://` | `wiki://index`                       | Índice principal (rawindex)  |
/// | `wiki://` | `wiki://page/{slug}`                 | Página da wiki               |
/// | `wiki://` | `wiki://list`                        | Lista de slugs               |
/// | `wiki://` | `wiki://rawindex`                    | Índice de raw sources        |
/// | `raw://`  | `raw://sources`                      | Lista de source IDs          |
/// | `raw://`  | `raw://source/{source_id}`           | Conteúdo bruto da source     |
/// | `raw://`  | `raw://sourcemetadata/{source_id}`   | Metadados da source          |
#[derive(Debug, Clone, PartialEq, Eq)]
enum WikiUri {
    Log,
    Index,
    Page(String),
    List,
    RawIndex,
    RawSources,
    RawSource(String),
    RawSourceMetadata(String),
}

impl WikiUri {
    /// Faz o parse de uma string de URI lógica (formato `esquema://path`).
    fn parse(uri: &str) -> Option<Self> {
        if let Some(rest) = uri.strip_prefix("wiki://") {
            match rest {
                "log" => return Some(WikiUri::Log),
                "index" => return Some(WikiUri::Index),
                "list" => return Some(WikiUri::List),
                "rawindex" => return Some(WikiUri::RawIndex),
                _ => {}
            }
            if let Some(slug) = rest.strip_prefix("page/") {
                return Some(WikiUri::Page(slug.to_string()));
            }
        }

        if let Some(rest) = uri.strip_prefix("raw://") {
            match rest {
                "sources" => return Some(WikiUri::RawSources),
                _ => {}
            }
            if let Some(source_id) = rest.strip_prefix("sourcemetadata/") {
                return Some(WikiUri::RawSourceMetadata(source_id.to_string()));
            }
            if let Some(source_id) = rest.strip_prefix("source/") {
                return Some(WikiUri::RawSource(source_id.to_string()));
            }
        }

        None
    }
}

impl WikiFileManager {
    /// Cria um novo `WikiFileManager`.
    ///
    /// Se `root` não for informado, usa o diretório corrente (`std::env::current_dir`).
    /// O diretório `.advwiki/` é resolvido como `{root}/.advwiki`.
    pub fn new(root: Option<PathBuf>) -> Self {
        let root = root.unwrap_or_else(|| {
            std::env::current_dir().expect("Falha ao obter diretório corrente")
        });
        let wiki_dir = root.join(".advwiki");
        Self { root, wiki_dir }
    }

    /// Retorna uma referência ao diretório raiz do projeto.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Retorna uma referência ao diretório `.advwiki/`.
    pub fn wiki_dir(&self) -> &Path {
        &self.wiki_dir
    }


    /// Resolve uma URI lógica para um `PathBuf` físico **validado**.
    ///
    /// A validação consiste em:
    /// 1. Fazer o parse da URI em um `WikiUri`.
    /// 2. Construir o caminho candidato a partir do `root` / `wiki_dir`.
    /// 3. Canonicalizar o caminho (resolve `.` e `..`) e verificar se o
    ///    resultado ainda está dentro de `root`. Se não estiver, rejeita
    ///    com erro de path traversal.
    ///
    /// Retorna `None` se a URI não for reconhecida.
    async fn resolve_uri(&self, uri: &str) -> Option<anyhow::Result<PathBuf>> {
        let parsed = WikiUri::parse(uri)?;

        let candidate = match &parsed {
            WikiUri::Log => self.root.join(".advwikilog.md"),
            WikiUri::Index => self.root.join("rawindex.md"),
            WikiUri::Page(slug) => self.wiki_dir.join("pages").join(format!("{slug}.md")),
            WikiUri::List => {
                // List não é um arquivo, mas um diretório — retornamos o dir para listagem
                return Some(Ok(self.wiki_dir.join("pages")));
            }
            WikiUri::RawIndex => self.root.join("rawindex.md"),
            WikiUri::RawSources => {
                return Some(Ok(self.wiki_dir.join("sources")));
            }
            WikiUri::RawSource(source_id) => {
                self.wiki_dir.join("sources").join(source_id)
            }
            WikiUri::RawSourceMetadata(source_id) => {
                self.wiki_dir
                    .join("metadata")
                    .join(format!("{source_id}.json"))
            }
        };

        Some(self.validate_path(candidate).await)
    }

    /// Valida que `path` está dentro de `self.root`, protegendo contra
    /// path traversal. Canonicaliza e verifica o prefixo.
    async fn validate_path(&self, path: PathBuf) -> anyhow::Result<PathBuf> {
        // Se o caminho não existe ainda (ex: vamos criar um arquivo novo),
        // validamos o diretório pai.
        let path_to_check = if path.exists() {
            fs::canonicalize(&path).await?
        } else {
            // Sobe até o primeiro ancestral que existe
            let mut ancestor = path.clone();
            loop {
                if ancestor.exists() {
                    break;
                }
                if !ancestor.pop() {
                    // Chegou na raiz sem encontrar ancestral — isso não deveria
                    // acontecer se o root existe. Deixamos passar e validamos
                    // contra o root canônico depois.
                    break;
                }
            }
            let canonical_ancestor = fs::canonicalize(&ancestor).await?;
            // Reconstrói o restante do caminho
            if let Ok(relative) = path.strip_prefix(&ancestor) {
                canonical_ancestor.join(relative)
            } else {
                // Fallback: canonicaliza o que der
                canonical_ancestor
            }
        };

        let canonical_root = fs::canonicalize(&self.root)
            .await
            .context("Falha ao canonicalizar o diretório raiz")?;

        if !path_to_check.starts_with(&canonical_root) {
            bail!(
                "Path traversal detectado: '{}' está fora do diretório raiz '{}'",
                path_to_check.display(),
                canonical_root.display()
            );
        }

        // Retornamos o caminho original (não canonicalizado) para preservar
        // a estrutura de diretórios lógica.
        Ok(path)
    }

    /// Inicializa a estrutura de diretórios da Wiki.
    ///
    /// Cria `.advwiki/pages/`, `.advwiki/sources/`, `.advwiki/metadata/`
    /// e os arquivos `.advwikilog.md` e `rawindex.md` se não existirem.
    pub async fn init(&self) -> anyhow::Result<()> {
        let dirs = [
            self.wiki_dir.clone(),
            self.wiki_dir.join("pages"),
            self.wiki_dir.join("sources"),
            self.wiki_dir.join("metadata"),
        ];

        for dir in &dirs {
            fs::create_dir_all(dir)
                .await
                .with_context(|| format!("Falha ao criar diretório: {}", dir.display()))?;
        }

        // Cria arquivos de log e índice se não existirem
        let log_path = self.root.join(".advwikilog.md");
        if !log_path.exists() {
            fs::write(&log_path, "# AdvWiki Log\n\n")
                .await
                .context("Falha ao criar arquivo de log")?;
        }

        let rawindex_path = self.root.join("rawindex.md");
        if !rawindex_path.exists() {
            fs::write(&rawindex_path, "# AdvWiki - Índice Principal\n\n")
                .await
                .context("Falha ao criar arquivo de índice raw")?;
        }

        tracing::info!(
            root = %self.root.display(),
            "Estrutura de diretórios da Wiki inicializada"
        );

        Ok(())
    }

    // ── Operações de Leitura ─────────────────────────────────────────────────

    /// Lê o log operacional (`.advwikilog.md`).
    pub async fn read_log(&self) -> anyhow::Result<String> {
        let path = self.root.join(".advwikilog.md");
        fs::read_to_string(&path)
            .await
            .with_context(|| format!("Falha ao ler log: {}", path.display()))
    }

    /// Lê uma página da wiki pelo slug.
    ///
    /// Resolve `wiki://page/{slug}` → `.advwiki/pages/{slug}.md`.
    pub async fn read_page(&self, slug: &str) -> anyhow::Result<String> {
        Self::validate_slug(slug)?;

        let uri = format!("wiki://page/{slug}");
        let path = self
            .resolve_uri(&uri)
            .await
            .context("URI inválida")?
            .context("Falha ao resolver URI")?;

        fs::read_to_string(&path)
            .await
            .with_context(|| format!("Falha ao ler página '{}': {}", slug, path.display()))
    }

    /// Retorna a lista de slugs (páginas) disponíveis.
    ///
    /// Lista o conteúdo de `.advwiki/pages/` e remove a extensão `.md`.
    pub async fn list_pages(&self) -> anyhow::Result<Vec<String>> {
        let pages_dir = self.wiki_dir.join("pages");
        let mut entries = fs::read_dir(&pages_dir)
            .await
            .with_context(|| format!("Falha ao listar diretório: {}", pages_dir.display()))?;

        let mut slugs = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(slug) = name.strip_suffix(".md") {
                        slugs.push(slug.to_string());
                    }
                }
            }
        }
        slugs.sort();
        Ok(slugs)
    }

    /// Lê o índice de raw sources (`rawindex.md`).
    pub async fn read_raw_index(&self) -> anyhow::Result<Vec<RawIndexEntry>> {
        let path = self.root.join("rawindex.md");
        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Falha ao ler rawindex: {}", path.display()))?;

        // parse ignora linhas de cabeçalho (começam com #) e linhas vazias.
        // cada entrada tem o formato: `source_id | original_path | extracted_at`
        let entries: Vec<RawIndexEntry> = content
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() >= 1 {
                    let source_id = parts[0].trim().to_string();
                    let original_path = parts.get(1).map(|s| s.trim().to_string());
                    let extracted_at = parts.get(2).map(|s| s.trim().to_string());
                    Some(RawIndexEntry {
                        source_id,
                        original_path: original_path.filter(|s| !s.is_empty()),
                        extracted_at: extracted_at.unwrap_or_default(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(entries)
    }

    /// Lista os IDs de raw sources disponíveis.
    ///
    /// Lista `.advwiki/sources/` e retorna os nomes dos arquivos/diretórios.
    pub async fn list_raw_sources(&self) -> anyhow::Result<Vec<String>> {
        let sources_dir = self.wiki_dir.join("sources");
        let mut entries = fs::read_dir(&sources_dir)
            .await
            .with_context(|| format!("Falha ao listar diretório: {}", sources_dir.display()))?;

        let mut ids = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Lê o conteúdo bruto de uma raw source.
    ///
    /// Resolve `raw://source/{source_id}` → `.advwiki/sources/{source_id}`.
    pub async fn read_raw_source(&self, source_id: &str) -> anyhow::Result<String> {
        Self::validate_slug(source_id)?;

        let uri = format!("raw://source/{source_id}");
        let path = self
            .resolve_uri(&uri)
            .await
            .context("URI inválida")?
            .context("Falha ao resolver URI")?;

        fs::read_to_string(&path)
            .await
            .with_context(|| {
                format!(
                    "Falha ao ler raw source '{}': {}",
                    source_id,
                    path.display()
                )
            })
    }

    /// Lê os metadados de uma raw source.
    ///
    /// Resolve `raw://sourcemetadata/{source_id}` → `.advwiki/metadata/{source_id}.json`.
    pub async fn read_raw_source_metadata(
        &self,
        source_id: &str,
    ) -> anyhow::Result<RawSourceMetadata> {
        Self::validate_slug(source_id)?;

        let uri = format!("raw://sourcemetadata/{source_id}");
        let path = self
            .resolve_uri(&uri)
            .await
            .context("URI inválida")?
            .context("Falha ao resolver URI")?;

        let json = fs::read_to_string(&path)
            .await
            .with_context(|| {
                format!(
                    "Falha ao ler metadados da source '{}': {}",
                    source_id,
                    path.display()
                )
            })?;

        serde_json::from_str(&json)
            .with_context(|| format!("Falha ao parsear JSON de metadados para '{}'", source_id))
    }


    /// Cria ou sobrescreve uma página da wiki.
    ///
    /// Resolve `wiki://page/{slug}` → `.advwiki/pages/{slug}.md`.
    pub async fn write_page(&self, slug: &str, content: &str) -> anyhow::Result<()> {
        // Valida o slug: apenas caracteres seguros para nome de arquivo
        Self::validate_slug(slug)?;

        let pages_dir = self.wiki_dir.join("pages");
        // Garante que o diretório pages existe
        fs::create_dir_all(&pages_dir).await?;

        let path = pages_dir.join(format!("{slug}.md"));

        // Valida segurança do caminho
        let _validated = self.validate_path(path.clone()).await?;

        fs::write(&path, content)
            .await
            .with_context(|| format!("Falha ao escrever página '{}': {}", slug, path.display()))?;

        tracing::info!(slug = %slug, "Página escrita");
        Ok(())
    }

    /// Adiciona uma entrada ao log operacional (`.advwikilog.md`).
    ///
    /// Cada entrada é prefixada com um timestamp ISO 8601.
    pub async fn append_to_log(&self, entry: &str) -> anyhow::Result<()> {
        let path = self.root.join(".advwikilog.md");
        let timestamp = chrono::Utc::now().to_rfc3339();
        let line = format!("- **{timestamp}**: {entry}\n");

        // Usa OpenOptions para append atômico
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .await
            .with_context(|| format!("Falha ao abrir log para append: {}", path.display()))?;

        file.write_all(line.as_bytes())
            .await
            .context("Falha ao escrever entrada no log")?;

        tracing::debug!(entry = %entry, "Entrada adicionada ao log");
        Ok(())
    }

    /// Salva uma raw source completa: conteúdo bruto + metadados JSON.
    ///
    /// - Conteúdo bruto → `.advwiki/sources/{source_id}`
    /// - Metadados → `.advwiki/metadata/{source_id}.json`
    /// - Atualiza `rawindex.md` com a nova entrada.
    pub async fn write_raw_source(
        &self,
        source_id: &str,
        metadata: &RawSourceMetadata,
        raw_content: &str,
    ) -> anyhow::Result<()> {
        // Valida o source_id
        Self::validate_slug(source_id)?;

        let sources_dir = self.wiki_dir.join("sources");
        let metadata_dir = self.wiki_dir.join("metadata");

        fs::create_dir_all(&sources_dir).await?;
        fs::create_dir_all(&metadata_dir).await?;

        let source_path = sources_dir.join(source_id);
        let metadata_path = metadata_dir.join(format!("{source_id}.json"));

        // Valida segurança dos caminhos
        let _ = self.validate_path(source_path.clone()).await?;
        let _ = self.validate_path(metadata_path.clone()).await?;

        // Serializa metadados
        let metadata_json = serde_json::to_string_pretty(metadata)
            .context("Falha ao serializar metadados para JSON")?;

        // Escreve conteúdo bruto e metadados em paralelo
        let write_source = fs::write(&source_path, raw_content);
        let write_metadata = fs::write(&metadata_path, &metadata_json);

        tokio::try_join!(write_source, write_metadata)
            .context("Falha ao escrever raw source ou metadados")?;

        // Atualiza o rawindex.md (upsert — sem duplicatas)
        self.upsert_raw_index_entry(source_id, &metadata.original_path, &metadata.extracted_at)
            .await?;

        tracing::info!(source_id = %source_id, "Raw source salva");
        Ok(())
    }

    /// Atualiza (upsert) uma entrada no `rawindex.md`.
    ///
    /// Lê todas as entradas existentes, remove qualquer duplicata do mesmo
    /// `source_id`, insere a nova entrada, ordena e reescreve o arquivo.
    async fn upsert_raw_index_entry(
        &self,
        source_id: &str,
        original_path: &Option<String>,
        extracted_at: &str,
    ) -> anyhow::Result<()> {
        let path = self.root.join("rawindex.md");

        let mut entries = self.read_raw_index().await.unwrap_or_default();

        // Remove entrada existente com mesmo source_id (evita duplicatas)
        entries.retain(|e| e.source_id != source_id);

        entries.push(RawIndexEntry {
            source_id: source_id.to_string(),
            original_path: original_path.clone(),
            extracted_at: extracted_at.to_string(),
        });

        entries.sort_by(|a, b| a.source_id.cmp(&b.source_id));

        let mut content = String::from("# AdvWiki - Índice Principal\n\n");
        for e in &entries {
            let original = e.original_path.as_deref().unwrap_or("-");
            content.push_str(&format!(
                "{} | {} | {}\n",
                e.source_id, original, e.extracted_at
            ));
        }

        fs::write(&path, content)
            .await
            .with_context(|| format!("Falha ao regravar rawindex: {}", path.display()))?;

        Ok(())
    }

    /// Exclui uma página da wiki pelo slug.
    #[allow(dead_code)]
    pub async fn delete_page(&self, slug: &str) -> anyhow::Result<()> {
        Self::validate_slug(slug)?;
        let path = self.wiki_dir.join("pages").join(format!("{slug}.md"));
        let _ = self.validate_path(path.clone()).await?;

        fs::remove_file(&path)
            .await
            .with_context(|| format!("Falha ao excluir página '{}': {}", slug, path.display()))?;

        tracing::info!(slug = %slug, "Página excluída");
        Ok(())
    }

    /// Exclui uma raw source e seus metadados.
    #[allow(dead_code)]
    pub async fn delete_raw_source(&self, source_id: &str) -> anyhow::Result<()> {
        Self::validate_slug(source_id)?;

        let source_path = self.wiki_dir.join("sources").join(source_id);
        let metadata_path = self.wiki_dir.join("metadata").join(format!("{source_id}.json"));

        let _ = self.validate_path(source_path.clone()).await?;
        let _ = self.validate_path(metadata_path.clone()).await?;

        // Remove ambos os arquivos (ignora erros se não existirem)
        let rm_source = async {
            if source_path.exists() {
                fs::remove_file(&source_path).await
            } else {
                Ok(())
            }
        };
        let rm_metadata = async {
            if metadata_path.exists() {
                fs::remove_file(&metadata_path).await
            } else {
                Ok(())
            }
        };

        tokio::try_join!(rm_source, rm_metadata)
            .context("Falha ao excluir raw source ou metadados")?;

        // Remove a entrada do rawindex.md
        self.upsert_raw_index_entry(source_id, &None, "").await?;
        let path = self.root.join("rawindex.md");
        let mut entries = self.read_raw_index().await.unwrap_or_default();
        entries.retain(|e| e.source_id != source_id);

        let mut content = String::from("# AdvWiki - Índice Principal\n\n");
        for e in &entries {
            let original = e.original_path.as_deref().unwrap_or("-");
            content.push_str(&format!(
                "{} | {} | {}\n",
                e.source_id, original, e.extracted_at
            ));
        }

        fs::write(&path, content)
            .await
            .with_context(|| format!("Falha ao regravar rawindex: {}", path.display()))?;

        tracing::info!(source_id = %source_id, "Raw source excluída");
        Ok(())
    }

    // ── Utilitários ──────────────────────────────────────────────────────────

    /// Valida que um slug/source_id contém apenas caracteres seguros para
    /// nome de arquivo: alfanuméricos, `-`, `_`, `.`.
    fn validate_slug(slug: &str) -> anyhow::Result<()> {
        if slug.is_empty() {
            bail!("Slug não pode ser vazio");
        }
        if slug.contains("..")
            || slug.contains('/')
            || slug.contains('\\')
            || slug.contains(':')
        {
            bail!(
                "Slug '{}' contém caracteres inválidos (.., /, \\, :) — path traversal bloqueado",
                slug
            );
        }
        // Permite apenas: a-z A-Z 0-9 - _ .
        if !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            bail!(
                "Slug '{}' contém caracteres não permitidos. Use apenas letras, números, -, _, .",
                slug
            );
        }
        Ok(())
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wiki_uri_parse_log() {
        assert_eq!(WikiUri::parse("wiki://log"), Some(WikiUri::Log));
    }

    #[test]
    fn test_wiki_uri_parse_index() {
        assert_eq!(WikiUri::parse("wiki://index"), Some(WikiUri::Index));
    }

    #[test]
    fn test_wiki_uri_parse_page() {
        assert_eq!(
            WikiUri::parse("wiki://page/home"),
            Some(WikiUri::Page("home".into()))
        );
        assert_eq!(
            WikiUri::parse("wiki://page/getting-started"),
            Some(WikiUri::Page("getting-started".into()))
        );
    }

    #[test]
    fn test_wiki_uri_parse_list() {
        assert_eq!(WikiUri::parse("wiki://list"), Some(WikiUri::List));
    }

    #[test]
    fn test_wiki_uri_parse_rawindex() {
        assert_eq!(WikiUri::parse("wiki://rawindex"), Some(WikiUri::RawIndex));
    }

    #[test]
    fn test_wiki_uri_parse_raw_sources() {
        assert_eq!(WikiUri::parse("raw://sources"), Some(WikiUri::RawSources));
    }

    #[test]
    fn test_wiki_uri_parse_raw_source() {
        assert_eq!(
            WikiUri::parse("raw://source/abc123"),
            Some(WikiUri::RawSource("abc123".into()))
        );
    }

    #[test]
    fn test_wiki_uri_parse_raw_source_metadata() {
        assert_eq!(
            WikiUri::parse("raw://sourcemetadata/abc123"),
            Some(WikiUri::RawSourceMetadata("abc123".into()))
        );
    }

    #[test]
    fn test_wiki_uri_parse_invalid() {
        assert_eq!(WikiUri::parse("invalid:uri"), None);
        assert_eq!(WikiUri::parse("wiki:"), None);
        assert_eq!(WikiUri::parse("wiki://"), None);
        assert_eq!(WikiUri::parse("raw://"), None);
        assert_eq!(WikiUri::parse(""), None);
    }

    #[test]
    fn test_validate_slug_valid() {
        assert!(WikiFileManager::validate_slug("home").is_ok());
        assert!(WikiFileManager::validate_slug("getting-started").is_ok());
        assert!(WikiFileManager::validate_slug("api_v2").is_ok());
        assert!(WikiFileManager::validate_slug("doc.md").is_ok());
    }

    #[test]
    fn test_validate_slug_invalid_traversal() {
        assert!(WikiFileManager::validate_slug("..").is_err());
        assert!(WikiFileManager::validate_slug("../etc").is_err());
        assert!(WikiFileManager::validate_slug("a/b").is_err());
        assert!(WikiFileManager::validate_slug("a\\b").is_err());
        assert!(WikiFileManager::validate_slug("a:b").is_err());
    }

    #[test]
    fn test_validate_slug_invalid_chars() {
        assert!(WikiFileManager::validate_slug("hello world").is_err());
        assert!(WikiFileManager::validate_slug("tag<evil>").is_err());
    }

    #[test]
    fn test_validate_slug_empty() {
        assert!(WikiFileManager::validate_slug("").is_err());
    }

    #[test]
    fn test_raw_source_metadata_new() {
        let meta = RawSourceMetadata::new("test-id".into(), 1024);
        assert_eq!(meta.source_id, "test-id");
        assert_eq!(meta.size_bytes, 1024);
        assert!(meta.original_path.is_none());
        assert!(meta.mime_type.is_none());
        assert!(!meta.extracted_at.is_empty());
        assert!(meta.tags.is_empty());
    }
}
