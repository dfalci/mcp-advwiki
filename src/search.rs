// ── Módulo de Busca com Tantivy (BM25) ─────────────────────────────────────
//
// Motor de busca local que indexa páginas e raw sources da Wiki usando o
// algoritmo BM25. Consome eventos do `WikiWatcher` para manter o índice
// atualizado de forma reativa.
//
// Schema do índice:
//
//   Campo          | Tipo    | Indexed | Stored | Fast | Descrição
//   ---------------|---------|---------|--------|------|------------------
//   uri            | STRING  | ✓       | ✓      |      | Chave primária (ex: wiki://page/home)
//   title          | TEXT    | ✓       | ✓      |      | Título do documento
//   content        | TEXT    | ✓       | ✓      |      | Conteúdo textual completo
//   last_modified  | I64     | ✓       | ✓      | ✓    | Timestamp Unix (segundos)

use anyhow::Context;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::*;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

// rstruturas de dados

/// resultado de uma busca.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// URI lógica do documento (ex: `wiki://page/home`).
    pub uri: String,
    /// Título do documento.
    pub title: String,
    /// Score BM25 (quanto maior, mais relevante).
    pub score: f32,
    /// Trecho do conteúdo (primeiros 300 caracteres).
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Page,
    Raw,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Raw => "raw",
        }
    }
}

fn escape_for_tantivy(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for ch in query.chars() {
        if matches!(
            ch,
            '+' | '-' | '&' | '|' | '!' | '(' | ')' | '{' | '}' | '[' | ']' | '^' | '"' | '~' | '*' | '?' | ':' | '\\' | '/'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// motor de busca da Wiki baseado em Tantivy.
///
/// # Thread safety
///
/// - `IndexWriter` é `Send` mas não `Sync` — protegido por `Arc<Mutex<>>`.
/// - `IndexReader` é `Send + Sync + Clone` — leituras concorrentes sem lock.
pub struct WikiSearchEngine {
    /// Índice Tantivy em disco.
    index: Index,
    /// Caminho do diretório do índice.
    index_path: PathBuf,
    /// Handles para os campos do schema (mesmos IDs do índice).
    fields: Fields,
    /// Writer com acesso exclusivo (mutex).
    writer: Arc<Mutex<IndexWriter>>,
    /// Reader para buscas concorrentes.
    reader: IndexReader,
}

// acesso aos campos

/// Wrapper para acesso tipado aos campos do schema.
///
/// Criado uma vez na inicialização e clonado para uso em operações de
/// leitura/escrita.
#[derive(Clone)]
struct Fields {
    uri: Field,
    title: Field,
    content: Field,
    kind: Field,
    last_modified: Field,
}

impl Fields {
    /// constrói o schema Tantivy e retorna os campos + o schema.
    fn build() -> (Self, Schema) {
        let mut schema_builder = Schema::builder();

        // STRING: valor bruto, indexado como termo único, não tokenizado
        let uri = schema_builder.add_text_field("uri", STRING | STORED);
        // TEXT: tokenizado (default tokenizer)
        let title = schema_builder.add_text_field("title", TEXT | STORED);
        let content = schema_builder.add_text_field("content", TEXT | STORED);
        let kind = schema_builder.add_text_field("kind", STRING | STORED);
        // I64: timestamp Unix em segundos, fast field para ordenação
        let last_modified = schema_builder.add_i64_field("last_modified", INDEXED | STORED | FAST);

        let schema = schema_builder.build();

        (
            Self {
                uri,
                title,
                content,
                kind,
                last_modified,
            },
            schema,
        )
    }
}


impl WikiSearchEngine {
    /// abre um indice existente ou cria um novo no caminho especificado.
    /// O diretório de índice é criado automaticamente se não existir.
    pub fn new(index_path: PathBuf) -> anyhow::Result<Self> {
        let (fields, schema) = Fields::build();

        // abre ou cria o diretório do índice
        let index = if index_path.exists() {
            match Index::open_in_dir(&index_path) {
                Ok(index) => index,
                Err(error) => {
                    tracing::warn!(
                        path = %index_path.display(),
                        error = %error,
                        "Falha ao abrir índice existente; recriando diretório do índice"
                    );
                    std::fs::remove_dir_all(&index_path).with_context(|| {
                        format!("Falha ao remover diretório do índice incompatível: {}", index_path.display())
                    })?;
                    std::fs::create_dir_all(&index_path)
                        .with_context(|| format!("Falha ao recriar diretório do índice: {}", index_path.display()))?;
                    Index::create_in_dir(&index_path, schema.clone())
                        .with_context(|| format!("Falha ao recriar índice em: {}", index_path.display()))?
                }
            }
        } else {
            std::fs::create_dir_all(&index_path)
                .with_context(|| format!("Falha ao criar diretório do índice: {}", index_path.display()))?;
            Index::create_in_dir(&index_path, schema.clone())
                .with_context(|| format!("Falha ao criar índice em: {}", index_path.display()))?
        };

        // writer com 50 MB de buffer de RAM
        let writer = index
            .writer(50_000_000)
            .context("Falha ao criar IndexWriter")?;
        let writer = Arc::new(Mutex::new(writer));

        // reader para buscas concorrentes
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("Falha ao criar IndexReader")?;

        tracing::info!(
            path = %index_path.display(),
            "Índice Tantivy inicializado"
        );

        Ok(Self {
            index,
            index_path,
            fields,
            writer,
            reader,
        })
    }

    /// retorna o caminho do diretório do índice.
    pub fn index_path(&self) -> &PathBuf {
        &self.index_path
    }

    // ── Operações de Escrita ─────────────────────────────────────────────────

    /// indexa ou atualiza um documento no índice.
    ///
    /// se já existir um documento com a mesma `uri`, ele é removido antes
    /// da inserção (upsert semântico).
    ///
    /// - `uri`: Chave primária (ex: `wiki://page/home`).
    /// - `title`: Título do documento.
    /// - `content`: Conteúdo textual completo.
    /// - `last_modified`: Timestamp Unix em segundos da última modificação.
    pub fn index_document(
        &self,
        kind: DocumentKind,
        uri: &str,
        title: &str,
        content: &str,
        last_modified: i64,
    ) -> anyhow::Result<()> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();

        // remove documento existente com a mesma URI (upsert)
        let uri_term = Term::from_field_text(fields.uri, uri);
        writer.delete_term(uri_term);

        // adiciona o novo documento
        writer
            .add_document(doc!(
                fields.uri => uri,
                fields.title => title,
                fields.content => content,
                fields.kind => kind.as_str(),
                fields.last_modified => last_modified,
            ))
            .with_context(|| format!("Falha ao indexar documento: {}", uri))?;

        // commit para persistir e tornar visível para buscas
        writer
            .commit()
            .context("Falha ao commitar alterações no índice")?;

        tracing::debug!(uri = %uri, "Documento indexado");
        Ok(())
    }

    /// remove um documento do índice pela URI.
    pub fn delete_document(&self, uri: &str) -> anyhow::Result<()> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();

        let uri_term = Term::from_field_text(fields.uri, uri);
        writer.delete_term(uri_term);

        writer
            .commit()
            .context("Falha ao commitar remoção no índice")?;

        tracing::debug!(uri = %uri, "Documento removido do índice");
        Ok(())
    }

    /// roda o clear no índice, removendo todos os documentos.
    pub fn clear(&self) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.delete_all_documents()?;
        writer.commit()?;
        Ok(())
    }

    /// indexa todos os documentos de uma vez (bulk index).
    ///
    /// útil para rebuild completo do índice a partir do disco.
    /// cada tupla contém `(kind, uri, title, content, last_modified)`.
    pub fn index_bulk(&self, documents: &[(DocumentKind, String, String, String, i64)]) -> anyhow::Result<u64> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();
        let mut count = 0u64;

        for (kind, uri, title, content, last_modified) in documents {
            // Remove documento existente
            let uri_term = Term::from_field_text(fields.uri, uri);
            writer.delete_term(uri_term);

            writer.add_document(doc!(
                fields.uri => uri.as_str(),
                fields.title => title.as_str(),
                fields.content => content.as_str(),
                fields.kind => kind.as_str(),
                fields.last_modified => *last_modified,
            ))?;
            count += 1;
        }

        writer
            .commit()
            .context("Falha ao commitar bulk index")?;

        tracing::info!(count = %count, "Bulk index concluído");
        Ok(count)
    }

    /// Realiza uma busca textual no índice.
    ///
    /// A query é parseada usando o `QueryParser` padrão do Tantivy,
    /// que suporta termos, frases entre aspas, e operadores booleanos.
    ///
    /// - `query_str`: Texto da busca (ex: `"rust programming"`).
    /// - `limit`: Número máximo de resultados.
    ///
    /// Retorna os resultados ordenados por score BM25 (decrescente).
    pub fn search(&self, query_str: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        self.search_with_kind_filter(query_str, limit, None)
    }

    pub fn search_with_kind_filter(
        &self,
        query_str: &str,
        limit: usize,
        kind: Option<DocumentKind>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let fields = &self.fields;
        let reader = &self.reader;

        // Garante que o reader veja os commits mais recentes
        reader.reload().context("Falha ao recarregar o reader do índice")?;

        let searcher = reader.searcher();

        let query: Box<dyn Query> = if query_str.trim().is_empty() {
            Box::new(AllQuery)
        } else {
            let query_parser = QueryParser::for_index(&self.index, vec![fields.title, fields.content]);
            let parsed = query_parser.parse_query(query_str).or_else(|_| {
                let escaped = escape_for_tantivy(query_str);
                query_parser.parse_query(&escaped)
            });
            Box::new(parsed.with_context(|| format!("Falha ao parsear query: '{}'", query_str))?)
        };

        let query: Box<dyn Query> = if let Some(kind) = kind {
            let kind_term = Term::from_field_text(fields.kind, kind.as_str());
            Box::new(BooleanQuery::from(vec![
                (Occur::Must, query),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(kind_term, IndexRecordOption::Basic)),
                ),
            ]))
        } else {
            query
        };

        // Busca top-K documentos ordenados por score BM25
        let collector = TopDocs::with_limit(limit).order_by_score();
        let top_docs: Vec<(tantivy::Score, tantivy::DocAddress)> = searcher
            .search(query.as_ref(), &collector)
            .context("Falha ao executar busca")?;

        let mut results = Vec::with_capacity(top_docs.len());

        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher
                .doc(doc_address)
                .context("Falha ao recuperar documento do índice")?;

            let uri = doc
                .get_first(fields.uri)
                .and_then(|v| v.as_str())
                .unwrap_or("(sem uri)")
                .to_string();

            let title = doc
                .get_first(fields.title)
                .and_then(|v| v.as_str())
                .unwrap_or("(sem título)")
                .to_string();

            let full_content = doc
                .get_first(fields.content)
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Snippet: primeiros 300 caracteres do conteúdo
            let snippet = if full_content.len() > 300 {
                let end = full_content
                    .char_indices()
                    .nth(300)
                    .map(|(i, _)| i)
                    .unwrap_or(full_content.len());
                format!("{}...", &full_content[..end])
            } else {
                full_content.to_string()
            };

            results.push(SearchResult {
                uri,
                title,
                score,
                snippet,
            });
        }

        Ok(results)
    }

    /// Retorna o número total de documentos no índice.
    pub fn doc_count(&self) -> anyhow::Result<u64> {
        self.reader.reload().context("Falha ao recarregar reader")?;
        let searcher = self.reader.searcher();
        Ok(searcher.num_docs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Cria um motor de busca em diretório temporário para testes.
    fn test_engine() -> (WikiSearchEngine, TempDir) {
        let dir = TempDir::new().expect("Falha ao criar diretório temporário");
        let index_path = dir.path().join("test_index");
        let engine = WikiSearchEngine::new(index_path).expect("Falha ao criar engine de teste");
        (engine, dir)
    }

    #[test]
    fn test_index_and_search_single_document() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(DocumentKind::Page, "wiki://page/home", "Home", "Bem-vindo à AdvWiki!", 1000)
            .unwrap();

        let results = engine.search("Bem-vindo", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "wiki://page/home");
        assert_eq!(results[0].title, "Home");
        assert!(results[0].score > 0.0);
        assert!(results[0].snippet.contains("Bem-vindo"));
    }

    #[test]
    fn test_search_multiple_documents() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/rust",
                "Rust Lang",
                "Rust é uma linguagem de programação systems.",
                1000,
            )
            .unwrap();
        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/python",
                "Python",
                "Python é uma linguagem de scripting.",
                1000,
            )
            .unwrap();

        let results = engine.search("linguagem", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_upsert_document() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(DocumentKind::Page, "wiki://page/api", "API v1", "Conteúdo antigo", 1000)
            .unwrap();
        engine
            .index_document(DocumentKind::Page, "wiki://page/api", "API v2", "Conteúdo novo atualizado", 2000)
            .unwrap();

        let results = engine.search("atualizado", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "API v2");

        // Verifica que não há duplicatas
        assert_eq!(engine.doc_count().unwrap(), 1);
    }

    #[test]
    fn test_delete_document() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/temp",
                "Temp",
                "Página temporária para teste de remoção",
                1000,
            )
            .unwrap();

        assert_eq!(engine.doc_count().unwrap(), 1);

        engine.delete_document("wiki://page/temp").unwrap();

        assert_eq!(engine.doc_count().unwrap(), 0);

        let results = engine.search("temporária", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_no_results() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(DocumentKind::Page, "wiki://page/only", "Only", "Apenas esta página", 1000)
            .unwrap();

        let results = engine.search("inexistente", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_bulk_index() {
        let (engine, _dir) = test_engine();

        let docs = vec![
            (
                DocumentKind::Page,
                "wiki://page/a".to_string(),
                "A".to_string(),
                "Conteúdo A".to_string(),
                1000i64,
            ),
            (
                DocumentKind::Page,
                "wiki://page/b".to_string(),
                "B".to_string(),
                "Conteúdo B".to_string(),
                1000i64,
            ),
            (
                DocumentKind::Page,
                "wiki://page/c".to_string(),
                "C".to_string(),
                "Conteúdo C".to_string(),
                1000i64,
            ),
        ];

        engine.clear().unwrap();
        let count = engine.index_bulk(&docs).unwrap();
        assert_eq!(count, 3);
        assert_eq!(engine.doc_count().unwrap(), 3);

        let results = engine.search("Conteúdo", 10).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_empty_query() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(DocumentKind::Page, "wiki://page/x", "X", "abc", 1000)
            .unwrap();

        let results = engine.search("", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "wiki://page/x");
    }

    #[test]
    fn test_snippet_truncation() {
        let (engine, _dir) = test_engine();

        let long_content = "A".repeat(500);
        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/long",
                "Long Page",
                &long_content,
                1000,
            )
            .unwrap();

        let results = engine.search("Long", 5).unwrap();
        assert_eq!(results.len(), 1);
        // Snippet deve terminar com "..."
        assert!(results[0].snippet.ends_with("..."));
        // Snippet deve ter no máximo ~303 caracteres (300 + "...")
        assert!(results[0].snippet.len() <= 303);
    }

    #[test]
    fn test_search_with_kind_filter_returns_only_pages() {
        let (engine, _dir) = test_engine();

        for i in 0..3 {
            engine
                .index_document(
                    DocumentKind::Raw,
                    &format!("raw://source/{i}"),
                    &format!("raw-{i}"),
                    "alpha alpha alpha alpha alpha",
                    1000,
                )
                .unwrap();
        }

        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/alpha",
                "Alpha Page",
                "alpha de verdade na página",
                1000,
            )
            .unwrap();

        let results = engine
            .search_with_kind_filter("alpha", 1, Some(DocumentKind::Page))
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "wiki://page/alpha");
    }

    #[test]
    fn test_search_falls_back_to_escaped_natural_query() {
        let (engine, _dir) = test_engine();

        engine
            .index_document(
                DocumentKind::Page,
                "wiki://page/fallback",
                "fallback query",
                "conteúdo com query natural",
                1000,
            )
            .unwrap();

        let results = engine.search("fallback \"query", 5).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "wiki://page/fallback");
    }
}
