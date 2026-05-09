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
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

// ── Estruturas de Dados ─────────────────────────────────────────────────────

/// Resultado de uma busca.
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

/// Motor de busca da Wiki baseado em Tantivy.
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

// ── Acesso aos Campos ───────────────────────────────────────────────────────

/// Wrapper para acesso tipado aos campos do schema.
///
/// Criado uma vez na inicialização e clonado para uso em operações de
/// leitura/escrita.
#[derive(Clone)]
struct Fields {
    uri: Field,
    title: Field,
    content: Field,
    last_modified: Field,
}

impl Fields {
    /// Constrói o schema Tantivy e retorna os campos + o Schema.
    fn build() -> (Self, Schema) {
        let mut schema_builder = Schema::builder();

        // STRING: valor bruto, indexado como termo único, não tokenizado
        let uri = schema_builder.add_text_field("uri", STRING | STORED);
        // TEXT: tokenizado (default tokenizer)
        let title = schema_builder.add_text_field("title", TEXT | STORED);
        let content = schema_builder.add_text_field("content", TEXT | STORED);
        // I64: timestamp Unix em segundos, fast field para ordenação
        let last_modified = schema_builder.add_i64_field("last_modified", INDEXED | STORED | FAST);

        let schema = schema_builder.build();

        (
            Self {
                uri,
                title,
                content,
                last_modified,
            },
            schema,
        )
    }
}

// ── Construtor ──────────────────────────────────────────────────────────────

impl WikiSearchEngine {
    /// Abre um índice existente ou cria um novo no caminho especificado.
    ///
    /// O diretório de índice é criado automaticamente se não existir.
    pub fn new(index_path: PathBuf) -> anyhow::Result<Self> {
        let (fields, schema) = Fields::build();

        // Abre ou cria o diretório do índice
        let index = if index_path.exists() {
            Index::open_in_dir(&index_path)
                .with_context(|| format!("Falha ao abrir índice em: {}", index_path.display()))?
        } else {
            std::fs::create_dir_all(&index_path)
                .with_context(|| format!("Falha ao criar diretório do índice: {}", index_path.display()))?;
            Index::create_in_dir(&index_path, schema)
                .with_context(|| format!("Falha ao criar índice em: {}", index_path.display()))?
        };

        // Writer com 50 MB de buffer de RAM
        let writer = index
            .writer(50_000_000)
            .context("Falha ao criar IndexWriter")?;
        let writer = Arc::new(Mutex::new(writer));

        // Reader para buscas concorrentes
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

    /// Retorna o caminho do diretório do índice.
    pub fn index_path(&self) -> &PathBuf {
        &self.index_path
    }

    // ── Operações de Escrita ─────────────────────────────────────────────────

    /// Indexa ou atualiza um documento no índice.
    ///
    /// Se já existir um documento com a mesma `uri`, ele é removido antes
    /// da inserção (upsert semântico).
    ///
    /// - `uri`: Chave primária (ex: `wiki://page/home`).
    /// - `title`: Título do documento.
    /// - `content`: Conteúdo textual completo.
    /// - `last_modified`: Timestamp Unix em segundos da última modificação.
    pub fn index_document(
        &self,
        uri: &str,
        title: &str,
        content: &str,
        last_modified: i64,
    ) -> anyhow::Result<()> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();

        // Remove documento existente com a mesma URI (upsert)
        let uri_term = tantivy::Term::from_field_text(fields.uri, uri);
        writer.delete_term(uri_term);

        // Adiciona o novo documento
        writer
            .add_document(doc!(
                fields.uri => uri,
                fields.title => title,
                fields.content => content,
                fields.last_modified => last_modified,
            ))
            .with_context(|| format!("Falha ao indexar documento: {}", uri))?;

        // Commit para persistir e tornar visível para buscas
        writer
            .commit()
            .context("Falha ao commitar alterações no índice")?;

        tracing::debug!(uri = %uri, "Documento indexado");
        Ok(())
    }

    /// Remove um documento do índice pela URI.
    pub fn delete_document(&self, uri: &str) -> anyhow::Result<()> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();

        let uri_term = tantivy::Term::from_field_text(fields.uri, uri);
        writer.delete_term(uri_term);

        writer
            .commit()
            .context("Falha ao commitar remoção no índice")?;

        tracing::debug!(uri = %uri, "Documento removido do índice");
        Ok(())
    }

    /// Indexa todos os documentos de uma vez (bulk index).
    ///
    /// Útil para rebuild completo do índice a partir do disco.
    /// Cada tupla contém `(uri, title, content, last_modified)`.
    pub fn index_bulk(&self, documents: &[(String, String, String, i64)]) -> anyhow::Result<u64> {
        let fields = &self.fields;
        let mut writer = self.writer.lock().unwrap();
        let mut count = 0u64;

        for (uri, title, content, last_modified) in documents {
            // Remove documento existente
            let uri_term = tantivy::Term::from_field_text(fields.uri, uri);
            writer.delete_term(uri_term);

            writer.add_document(doc!(
                fields.uri => uri.as_str(),
                fields.title => title.as_str(),
                fields.content => content.as_str(),
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

    // ── Operações de Busca ───────────────────────────────────────────────────

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
        let fields = &self.fields;
        let reader = &self.reader;

        // Garante que o reader veja os commits mais recentes
        reader.reload().context("Falha ao recarregar o reader do índice")?;

        let searcher = reader.searcher();

        // Query parser nos campos title e content
        let query_parser = QueryParser::for_index(&self.index, vec![fields.title, fields.content]);
        let query = query_parser
            .parse_query(query_str)
            .with_context(|| format!("Falha ao parsear query: '{}'", query_str))?;

        // Busca top-K documentos ordenados por score BM25
        let collector = TopDocs::with_limit(limit).order_by_score();
        let top_docs: Vec<(tantivy::Score, tantivy::DocAddress)> = searcher
            .search(&query, &collector)
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
            .index_document("wiki://page/home", "Home", "Bem-vindo à AdvWiki!", 1000)
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
                "wiki://page/rust",
                "Rust Lang",
                "Rust é uma linguagem de programação systems.",
                1000,
            )
            .unwrap();
        engine
            .index_document(
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
            .index_document("wiki://page/api", "API v1", "Conteúdo antigo", 1000)
            .unwrap();
        engine
            .index_document("wiki://page/api", "API v2", "Conteúdo novo atualizado", 2000)
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
            .index_document("wiki://page/only", "Only", "Apenas esta página", 1000)
            .unwrap();

        let results = engine.search("inexistente", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_bulk_index() {
        let (engine, _dir) = test_engine();

        let docs = vec![
            (
                "wiki://page/a".to_string(),
                "A".to_string(),
                "Conteúdo A".to_string(),
                1000i64,
            ),
            (
                "wiki://page/b".to_string(),
                "B".to_string(),
                "Conteúdo B".to_string(),
                1000i64,
            ),
            (
                "wiki://page/c".to_string(),
                "C".to_string(),
                "Conteúdo C".to_string(),
                1000i64,
            ),
        ];

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
            .index_document("wiki://page/x", "X", "abc", 1000)
            .unwrap();

        // Query vazia: em Tantivy 0.26 pode retornar erro ou lista vazia
        match engine.search("", 5) {
            Ok(results) => assert!(results.is_empty(), "Query vazia deve retornar 0 resultados"),
            Err(_) => {} // erro de parse também é aceitável
        }
    }

    #[test]
    fn test_snippet_truncation() {
        let (engine, _dir) = test_engine();

        let long_content = "A".repeat(500);
        engine
            .index_document(
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
}
