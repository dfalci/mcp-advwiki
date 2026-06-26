// ── Ciclo de vida da indexação semântica (lado da ESCRITA) ──────────────────
//
// Conecta os módulos PUROS `embeddings` (chunking + provider) e `vector_store`
// (storage `.bin` + cache) ao runtime: um worker dedicado consome slugs de
// páginas, gera embeddings via API e persiste cada `.advwiki/embeddings/{slug}.bin`.
//
// Só a instância primary embeda (dona do writer/watcher). O worker espelha o
// `git_sync::spawn_committer`: canal `mpsc` + `tokio::spawn`, mas com concorrência
// fixa de 4 (via `Semaphore`) — embeddings são I/O de rede, valem paralelizar.
//
// Degradação aditiva: qualquer falha (leitura, API, escrita) loga e SEGUE — a
// página fica só com BM25, sem `.bin`. Nunca dá panic.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};

use crate::change_plan::content_hash;
use crate::embeddings::{EmbedError, EmbeddingProvider, SemanticConfig, chunk_page};
use crate::index_page::INDEX_SLUG;
use crate::storage::WikiFileManager;
use crate::vector_store::{ChunkVec, PageEmbeddings, VectorStore, write_page_embeddings};

/// Quantas páginas embedam em paralelo (embedding é I/O de rede).
const CONCURRENCY: usize = 4;
/// Tentativas em erro transitório antes de desistir (segue só com BM25).
const MAX_ATTEMPTS: usize = 3;
/// Backoff inicial entre tentativas; dobra a cada retry.
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Sobe o worker de embeddings e devolve o canal para enfileirar SLUGS de página.
/// Espelha `git_sync::spawn_committer`. A task consome slugs e processa até 4 em
/// paralelo; o slug de índice é ignorado (regenerado o tempo todo → churn). Quando
/// o canal fecha, a task encerra.
pub fn spawn_embedder(
    provider: Arc<dyn EmbeddingProvider>,
    store: Arc<VectorStore>,
    wiki: Arc<WikiFileManager>,
    cfg: SemanticConfig,
) -> mpsc::UnboundedSender<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let embeddings_dir = wiki.wiki_dir().join("embeddings");
    let sem = Arc::new(Semaphore::new(CONCURRENCY));

    tokio::spawn(async move {
        while let Some(slug) = rx.recv().await {
            if slug == INDEX_SLUG {
                continue;
            }
            // Adquire um permit ANTES de spawnar: limita a 4 em voo e dá
            // backpressure natural (o 5º slug espera um permit liberar).
            let permit = match sem.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break, // semáforo fechado — não ocorre na prática
            };
            let provider = provider.clone();
            let store = store.clone();
            let wiki = wiki.clone();
            let cfg = cfg.clone();
            let dir = embeddings_dir.clone();
            tokio::spawn(async move {
                let _permit = permit; // segura o permit até o slug terminar
                embed_slug(provider.as_ref(), &store, &wiki, &cfg, &dir, &slug).await;
            });
        }
        tracing::debug!("Worker de embeddings encerrado (canal fechado)");
    });

    tx
}

/// Varre as páginas e enfileira as que precisam (re)embedar (`.bin` faltando,
/// corpo mudou ou modelo divergente). Pula o slug de índice. Devolve quantas
/// foram enfileiradas — usado pelo boot para logar a contagem.
pub async fn enqueue_stale_pages(
    wiki: &WikiFileManager,
    store: &VectorStore,
    cfg: &SemanticConfig,
    tx: &mpsc::UnboundedSender<String>,
) -> usize {
    let slugs = match wiki.list_pages().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "embeddings: falha ao listar páginas no boot");
            return 0;
        }
    };

    let mut queued = 0usize;
    for slug in slugs {
        if slug == INDEX_SLUG {
            continue;
        }
        let content = match wiki.read_page(&slug).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(%slug, error = %e, "embeddings: falha ao ler página no scan — pulando");
                continue;
            }
        };
        let body = crate::frontmatter::strip_frontmatter(&content);
        let body_hash = content_hash(body);
        if needs_embed(store, &slug, &body_hash, &cfg.model) && tx.send(slug).is_ok() {
            queued += 1;
        }
    }
    queued
}

/// Apaga os embeddings de uma página: remove do cache e do disco. No-op se já
/// não existirem. Chamado no evento `PageDeleted`.
pub fn handle_delete(store: &VectorStore, embeddings_dir: &Path, slug: &str) {
    store.remove(slug);
    let path = embeddings_dir.join(format!("{slug}.bin"));
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::debug!(%slug, "embeddings: .bin removido"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(%slug, error = %e, "embeddings: falha ao remover .bin"),
    }
}

/// Embeda uma página e persiste o `.bin`. Aplica o gate de hash/modelo (não chama
/// a API se nada relevante mudou). Qualquer falha loga e retorna — a página segue
/// só com BM25.
async fn embed_slug(
    provider: &dyn EmbeddingProvider,
    store: &VectorStore,
    wiki: &WikiFileManager,
    cfg: &SemanticConfig,
    embeddings_dir: &Path,
    slug: &str,
) {
    let content = match wiki.read_page(slug).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(%slug, error = %e, "embeddings: falha ao ler página — pulando");
            return;
        }
    };
    let body = crate::frontmatter::strip_frontmatter(&content);
    let body_hash = content_hash(body);

    // GATE: corpo inalterado E mesmo modelo → nada a (re)fazer, sem chamar a API
    // (evita pagar por um save que só mexeu no `updated_at`).
    if !needs_embed(store, slug, &body_hash, &cfg.model) {
        tracing::trace!(%slug, "embeddings: já atualizado — pulando");
        return;
    }

    let chunks = chunk_page(&content, cfg);
    if chunks.is_empty() {
        // Página vazia (só frontmatter/espaços): some o `.bin` antigo se houver.
        handle_delete(store, embeddings_dir, slug);
        return;
    }
    let texts: Vec<String> = chunks
        .iter()
        .map(|c| body[c.start..c.end].to_string())
        .collect();

    let vectors = match embed_with_retry(provider, &texts).await {
        Some(v) => v,
        None => return, // já logado; página fica só com BM25
    };

    if vectors.len() != chunks.len() || vectors[0].is_empty() {
        tracing::warn!(
            %slug, esperado = chunks.len(), recebido = vectors.len(),
            "embeddings: resposta inconsistente da API — pulando"
        );
        return;
    }
    let dim = vectors[0].len() as u32;

    let chunk_vecs: Vec<ChunkVec> = chunks
        .iter()
        .zip(vectors)
        .map(|(c, vector)| ChunkVec {
            index: c.index,
            start: c.start,
            end: c.end,
            vector,
        })
        .collect();

    let page = PageEmbeddings {
        slug: slug.to_string(),
        dim,
        model: cfg.model.clone(),
        body_hash,
        chunks: chunk_vecs,
    };

    let path = embeddings_dir.join(format!("{slug}.bin"));
    if let Err(e) = write_page_embeddings(&path, &page) {
        tracing::error!(%slug, error = %e, "embeddings: falha ao gravar .bin — pulando");
        return;
    }
    store.upsert(page);
    tracing::debug!(%slug, dim, chunks = chunks.len(), "embeddings: página indexada");
}

/// Chama o provider com retry curto: erro `Transient` retenta com backoff;
/// `Permanent` desiste de imediato. `None` = desistiu (sempre logado).
async fn embed_with_retry(
    provider: &dyn EmbeddingProvider,
    texts: &[String],
) -> Option<Vec<Vec<f32>>> {
    let mut backoff = RETRY_BACKOFF;
    for attempt in 1..=MAX_ATTEMPTS {
        match provider.embed(texts).await {
            Ok(v) => return Some(v),
            Err(EmbedError::Permanent(msg)) => {
                tracing::warn!(error = %msg, "embeddings: erro permanente — não retenta");
                return None;
            }
            Err(EmbedError::Transient(msg)) => {
                if attempt == MAX_ATTEMPTS {
                    tracing::warn!(error = %msg, attempts = attempt, "embeddings: erro transitório persistente — desiste");
                    return None;
                }
                tracing::debug!(error = %msg, attempt, "embeddings: erro transitório — retry");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
        }
    }
    None
}

/// `true` se a página precisa (re)embedar: sem `.bin` em cache, corpo mudou
/// (hash) ou o `.bin` foi gerado por outro modelo. Só consulta o cache — não
/// chama a API.
fn needs_embed(store: &VectorStore, slug: &str, body_hash: &str, model: &str) -> bool {
    match store.descriptor(slug) {
        // `dim` é o do próprio `.bin` em cache, então aqui `is_compatible` recai
        // só na checagem de modelo (trocar o modelo invalida o embedding antigo).
        Some((dim, _)) => {
            store.get_hash(slug).as_deref() != Some(body_hash)
                || !store.is_compatible(slug, dim, model)
        }
        None => true,
    }
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{EmbedFuture, FakeEmbedder};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Provider que falha (Transient) as primeiras `fail_n` chamadas e depois
    /// delega ao `FakeEmbedder`. Conta as chamadas.
    struct FlakyEmbedder {
        inner: FakeEmbedder,
        fail_n: usize,
        calls: AtomicUsize,
    }

    impl FlakyEmbedder {
        fn new(fail_n: usize) -> Self {
            Self {
                inner: FakeEmbedder::new(),
                fail_n,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl EmbeddingProvider for FlakyEmbedder {
        fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbedFuture<'a> {
            Box::pin(async move {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n < self.fail_n {
                    Err(EmbedError::Transient("flaky".to_string()))
                } else {
                    self.inner.embed(inputs).await
                }
            })
        }
    }

    fn cfg() -> SemanticConfig {
        SemanticConfig::from_values(
            "k".to_string(),
            "http://localhost/v1".to_string(),
            "test-model".to_string(),
            2000,
        )
    }

    /// Monta uma wiki em tempdir com o diretório de embeddings já criado.
    async fn setup() -> (TempDir, Arc<WikiFileManager>, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let wiki = Arc::new(WikiFileManager::new(Some(dir.path().to_path_buf())));
        wiki.init().await.unwrap();
        let edir = wiki.wiki_dir().join("embeddings");
        std::fs::create_dir_all(&edir).unwrap();
        (dir, wiki, edir)
    }

    #[tokio::test]
    async fn test_embed_slug_writes_bin_and_populates_store() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "---\ntype: note\n---\nConteúdo da página alpha.")
            .await
            .unwrap();
        let provider = FakeEmbedder::new();
        let store = VectorStore::new();

        embed_slug(&provider, &store, &wiki, &cfg(), &edir, "alpha").await;

        assert!(edir.join("alpha.bin").exists(), ".bin deve ser gravado");
        assert_eq!(store.len(), 1, "store deve ter a página");
        assert!(store.get_hash("alpha").is_some());
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn test_unchanged_body_is_skipped_without_calling_provider() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "Corpo estável.").await.unwrap();
        let provider = FakeEmbedder::new();
        let store = VectorStore::new();
        let c = cfg();

        embed_slug(&provider, &store, &wiki, &c, &edir, "alpha").await;
        assert_eq!(provider.call_count(), 1, "primeira vez embeda");

        // Reenfileira com corpo inalterado → gate pula, provider NÃO é chamado.
        embed_slug(&provider, &store, &wiki, &c, &edir, "alpha").await;
        assert_eq!(provider.call_count(), 1, "corpo igual → sem nova chamada");
    }

    #[tokio::test]
    async fn test_model_change_forces_reembed() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "Corpo estável.").await.unwrap();
        let provider = FakeEmbedder::new();
        let store = VectorStore::new();

        embed_slug(&provider, &store, &wiki, &cfg(), &edir, "alpha").await;
        assert_eq!(provider.call_count(), 1);

        // Mesmo corpo, modelo diferente → stale → re-embeda.
        let other = SemanticConfig::from_values(
            "k".to_string(),
            "http://localhost/v1".to_string(),
            "outro-modelo".to_string(),
            2000,
        );
        embed_slug(&provider, &store, &wiki, &other, &edir, "alpha").await;
        assert_eq!(provider.call_count(), 2, "modelo divergente → re-embeda");
    }

    #[tokio::test]
    async fn test_handle_delete_removes_bin_and_store_entry() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "Conteúdo.").await.unwrap();
        let provider = FakeEmbedder::new();
        let store = VectorStore::new();
        embed_slug(&provider, &store, &wiki, &cfg(), &edir, "alpha").await;
        assert!(edir.join("alpha.bin").exists());

        handle_delete(&store, &edir, "alpha");

        assert!(!edir.join("alpha.bin").exists(), ".bin removido");
        assert!(store.get_hash("alpha").is_none(), "fora do store");
        // Idempotente: remover de novo não dá panic.
        handle_delete(&store, &edir, "alpha");
    }

    #[tokio::test]
    async fn test_transient_then_success_retries() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "Conteúdo.").await.unwrap();
        let provider = FlakyEmbedder::new(1); // falha 1x, depois OK
        let store = VectorStore::new();

        embed_slug(&provider, &store, &wiki, &cfg(), &edir, "alpha").await;

        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "1 falha + 1 sucesso");
        assert!(edir.join("alpha.bin").exists(), "acabou gravando após o retry");
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn test_permanent_error_skips_page_without_panic() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page("alpha", "Conteúdo.").await.unwrap();
        let provider = FakeEmbedder::failing(EmbedError::Permanent("4xx".to_string()));
        let store = VectorStore::new();

        embed_slug(&provider, &store, &wiki, &cfg(), &edir, "alpha").await;

        assert!(!edir.join("alpha.bin").exists(), "sem .bin");
        assert!(store.is_empty(), "página não entra no store");
        assert_eq!(provider.call_count(), 1, "permanente não retenta");
    }

    #[tokio::test]
    async fn test_index_slug_is_ignored_by_worker() {
        let (_d, wiki, edir) = setup().await;
        wiki.write_page(INDEX_SLUG, "Página de índice gerada.")
            .await
            .unwrap();
        let provider = Arc::new(FakeEmbedder::new());
        let store = Arc::new(VectorStore::new());

        let tx = spawn_embedder(provider.clone(), store.clone(), wiki.clone(), cfg());
        tx.send(INDEX_SLUG.to_string()).unwrap();
        // Também enfileira uma página real pra ter um marco observável.
        wiki.write_page("real", "Conteúdo real.").await.unwrap();
        tx.send("real".to_string()).unwrap();
        drop(tx);

        // Espera o worker drenar (sem .bin de index, com .bin de real).
        for _ in 0..50 {
            if edir.join("real.bin").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert!(edir.join("real.bin").exists(), "página real foi embedada");
        assert!(!edir.join(format!("{INDEX_SLUG}.bin")).exists(), "index é ignorado");
    }

    #[tokio::test]
    async fn test_enqueue_stale_pages_counts_and_skips_index() {
        let (_d, wiki, _edir) = setup().await;
        wiki.write_page("a", "Página A.").await.unwrap();
        wiki.write_page("b", "Página B.").await.unwrap();
        wiki.write_page(INDEX_SLUG, "Índice.").await.unwrap();
        let store = VectorStore::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let n = enqueue_stale_pages(&wiki, &store, &cfg(), &tx).await;
        drop(tx);

        assert_eq!(n, 2, "duas páginas reais, index fora");
        let mut got = Vec::new();
        while let Ok(s) = rx.try_recv() {
            got.push(s);
        }
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }
}
