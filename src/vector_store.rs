// ── Módulo de Vector Store (busca semântica, opt-in) ─────────────────────────
//
// Camada PURA de armazenamento e busca vetorial: formato em disco dos embeddings
// por página (`.advwiki/embeddings/{slug}.bin`, f32 little-endian), cache em RAM,
// cosseno força-bruta com max-pool por página e fusão RRF com o ranking BM25.
//
// Este módulo NÃO conhece provider/rede nem o módulo `embeddings.rs` — define seus
// próprios tipos (`ChunkVec`) de propósito, pra ficar desacoplado. A ponte entre
// os dois (gerar embeddings → gravar `.bin`) é de um card posterior.
//
// Responsabilidades:
//   - write/read_page_embeddings → roundtrip do `.bin` (header + chunks)
//   - VectorStore                → cache em RAM (load_all/upsert/remove/gates)
//   - cosine / semantic_search   → similaridade força-bruta + max-pool
//   - rrf_fuse                   → fusão de rankings (Reciprocal Rank Fusion)

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::RwLock;

// ── Formato em disco ──────────────────────────────────────────────────────────

/// Identificador do formato no início de cada `.bin`.
const MAGIC: &[u8; 4] = b"ADVE";
/// Versão do formato em disco. Incrementa em mudanças incompatíveis.
const FORMAT_VERSION: u8 = 1;

// ── Tipos em memória ──────────────────────────────────────────────────────────

/// Um chunk embedado: offsets em BYTES no corpo SEM frontmatter (sempre em
/// fronteira de char, então `&body[start..end]` é válido) e o vetor f32.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkVec {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub vector: Vec<f32>,
}

/// Todos os embeddings de uma página, prontos pra busca. `body_hash` é o MD5 hex
/// do corpo que os gerou (gate de re-embedding); `dim`/`model` validam o `.bin`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageEmbeddings {
    pub slug: String,
    pub dim: u32,
    pub model: String,
    pub body_hash: String,
    pub chunks: Vec<ChunkVec>,
}

/// Resultado de uma busca semântica: a página, seu score (max cosseno entre os
/// chunks) e os offsets do chunk vencedor (pro snippet, fatiado do corpo).
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticHit {
    pub slug: String,
    pub score: f32,
    pub best_index: usize,
    pub best_start: usize,
    pub best_end: usize,
}

// ── Constantes de fusão RRF ───────────────────────────────────────────────────

/// Constante de amortecimento do RRF (quanto maior, mais plano o decaimento).
pub const RRF_K: f64 = 60.0;
/// Peso do ranking semântico na fusão (preferencial sobre o léxico).
pub const W_SEM: f64 = 2.0;
/// Peso do ranking BM25 na fusão.
pub const W_BM25: f64 = 1.0;

// ── I/O do `.bin` ─────────────────────────────────────────────────────────────

/// Grava os embeddings de uma página em `path` (formato f32 little-endian).
/// O `slug` NÃO entra no arquivo — ele é codificado pelo nome `{slug}.bin`.
pub fn write_page_embeddings(path: &Path, page: &PageEmbeddings) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut w = BufWriter::new(file);

    w.write_all(MAGIC)?;
    w.write_all(&[FORMAT_VERSION])?;
    w.write_all(&page.dim.to_le_bytes())?;
    write_str(&mut w, &page.model)?;
    write_str(&mut w, &page.body_hash)?;
    w.write_all(&(page.chunks.len() as u32).to_le_bytes())?;

    for c in &page.chunks {
        w.write_all(&(c.index as u32).to_le_bytes())?;
        w.write_all(&(c.start as u32).to_le_bytes())?;
        w.write_all(&(c.end as u32).to_le_bytes())?;
        // Cada `.bin` tem dimensão fixa (`page.dim`); vetores divergentes seriam
        // um bug do produtor — corta no tamanho declarado por segurança.
        for &x in c.vector.iter().take(page.dim as usize) {
            w.write_all(&x.to_le_bytes())?;
        }
    }

    w.flush()?;
    Ok(())
}

/// Lê os embeddings de uma página de `path`. O `slug` vem do nome do arquivo
/// (`{slug}.bin`). Erros de formato viram `io::ErrorKind::InvalidData`.
pub fn read_page_embeddings(path: &Path) -> io::Result<PageEmbeddings> {
    let slug = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| invalid("nome de arquivo .bin inválido"))?;

    let file = std::fs::File::open(path)?;
    let mut r = BufReader::new(file);

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("magic inválido (não é um .bin do AdvWiki)"));
    }
    let version = read_u8(&mut r)?;
    if version != FORMAT_VERSION {
        return Err(invalid(&format!("versão de formato não suportada: {version}")));
    }
    let dim = read_u32(&mut r)?;
    let model = read_str(&mut r)?;
    let body_hash = read_str(&mut r)?;
    let n_chunks = read_u32(&mut r)? as usize;

    let mut chunks = Vec::with_capacity(n_chunks);
    for _ in 0..n_chunks {
        let index = read_u32(&mut r)? as usize;
        let start = read_u32(&mut r)? as usize;
        let end = read_u32(&mut r)? as usize;
        let mut vector = vec![0f32; dim as usize];
        for x in vector.iter_mut() {
            *x = read_f32(&mut r)?;
        }
        chunks.push(ChunkVec {
            index,
            start,
            end,
            vector,
        });
    }

    Ok(PageEmbeddings {
        slug,
        dim,
        model,
        body_hash,
        chunks,
    })
}

/// Grava uma string prefixada por comprimento `u16` (UTF-8).
fn write_str<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    let len: u16 = bytes
        .len()
        .try_into()
        .map_err(|_| invalid("string longa demais pro header"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

/// Lê uma string prefixada por comprimento `u16` (UTF-8).
fn read_str<R: Read>(r: &mut R) -> io::Result<String> {
    let len = read_u16(r)? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| invalid("string UTF-8 inválida no header"))
}

fn read_u8<R: Read>(r: &mut R) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16<R: Read>(r: &mut R) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_f32<R: Read>(r: &mut R) -> io::Result<f32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(f32::from_le_bytes(b))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// ── Cache em RAM ──────────────────────────────────────────────────────────────

/// Cache em memória dos embeddings de todas as páginas, indexado por slug.
/// Concorrência por `RwLock`: leituras (busca) em paralelo, escritas (upsert/
/// remove pelo worker) exclusivas.
pub struct VectorStore {
    pages: RwLock<HashMap<String, PageEmbeddings>>,
}

impl Default for VectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore {
    /// Store vazio.
    pub fn new() -> Self {
        Self {
            pages: RwLock::new(HashMap::new()),
        }
    }

    /// Varre `*.bin` em `embeddings_dir` e carrega o que conseguir ler. Arquivos
    /// ilegíveis/corrompidos são logados e ignorados (degradação aditiva: a página
    /// volta a aparecer só pelo BM25). Diretório ausente → store vazio.
    pub fn load_all(embeddings_dir: &Path) -> Self {
        let store = Self::new();
        let entries = match std::fs::read_dir(embeddings_dir) {
            Ok(e) => e,
            Err(_) => return store,
        };

        let mut loaded = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("bin") {
                continue;
            }
            match read_page_embeddings(&path) {
                Ok(page) => {
                    store.upsert(page);
                    loaded += 1;
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Embedding .bin ilegível — ignorado");
                }
            }
        }
        tracing::debug!(loaded, dir = %embeddings_dir.display(), "VectorStore carregado");
        store
    }

    /// Insere ou substitui os embeddings de uma página.
    pub fn upsert(&self, page: PageEmbeddings) {
        self.pages.write().unwrap().insert(page.slug.clone(), page);
    }

    /// Remove os embeddings de uma página (no-op se ausente).
    pub fn remove(&self, slug: &str) {
        self.pages.write().unwrap().remove(slug);
    }

    /// Hash MD5 do corpo que gerou os embeddings de `slug`, se houver. Gate de
    /// re-embedding: se o hash atual bate, não precisa re-embedar.
    pub fn get_hash(&self, slug: &str) -> Option<String> {
        self.pages
            .read()
            .unwrap()
            .get(slug)
            .map(|p| p.body_hash.clone())
    }

    /// `(dim, model)` do `.bin` em cache pra `slug`, se houver. Usado pelos gates
    /// de staleness do worker de indexação (mudança de modelo invalida o cache).
    pub fn descriptor(&self, slug: &str) -> Option<(u32, String)> {
        self.pages
            .read()
            .unwrap()
            .get(slug)
            .map(|p| (p.dim, p.model.clone()))
    }

    /// `true` se o `.bin` em cache pra `slug` foi gerado com a MESMA `dim` e
    /// `model` atuais. `false` (inclusive quando ausente) → stale: re-embedar.
    pub fn is_compatible(&self, slug: &str, dim: u32, model: &str) -> bool {
        self.pages
            .read()
            .unwrap()
            .get(slug)
            .map(|p| p.dim == dim && p.model == model)
            .unwrap_or(false)
    }

    /// Quantidade de páginas com embeddings em cache.
    pub fn len(&self) -> usize {
        self.pages.read().unwrap().len()
    }

    /// `true` se não há nenhuma página em cache.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Busca semântica força-bruta: cosseno da `query_vec` contra todos os chunks,
    /// max-pool por página (score = melhor chunk, guardando qual venceu). Retorna
    /// as `top_k` páginas por score desc. Páginas sem chunk não pontuam.
    pub fn semantic_search(&self, query_vec: &[f32], top_k: usize) -> Vec<SemanticHit> {
        let pages = self.pages.read().unwrap();
        let mut hits: Vec<SemanticHit> = Vec::new();

        for page in pages.values() {
            let mut best: Option<&ChunkVec> = None;
            let mut best_score = f32::NEG_INFINITY;
            for c in &page.chunks {
                let s = cosine(query_vec, &c.vector);
                if s > best_score {
                    best_score = s;
                    best = Some(c);
                }
            }
            if let Some(c) = best {
                hits.push(SemanticHit {
                    slug: page.slug.clone(),
                    score: best_score,
                    best_index: c.index,
                    best_start: c.start,
                    best_end: c.end,
                });
            }
        }

        // Ordena por score desc; desempate estável por slug pra determinismo.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        hits.truncate(top_k);
        hits
    }
}

// ── Cosseno ───────────────────────────────────────────────────────────────────

/// Similaridade de cosseno entre dois vetores. L2-normaliza por segurança (não
/// assume vetores já normalizados). Tamanhos diferentes ou vetor nulo → `0.0`.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ── Fusão RRF ─────────────────────────────────────────────────────────────────

/// Reciprocal Rank Fusion de dois rankings de slugs. Para cada slug, soma
/// `w / (k + rank)` em cada ranking onde aparece (rank 0-based) e ordena por
/// score desc. Devolve a ordem fundida de slugs (sem duplicatas). Desempate
/// estável: quem aparece antes no ranking semântico vence (peso pró-semântica).
pub fn rrf_fuse(
    bm25_order: &[String],
    semantic_order: &[String],
    k: f64,
    w_sem: f64,
    w_bm25: f64,
) -> Vec<String> {
    let mut scores: HashMap<&str, f64> = HashMap::new();
    // Ordem de descoberta pra desempate determinístico (semântica primeiro).
    let mut order: Vec<&str> = Vec::new();

    for (rank, slug) in semantic_order.iter().enumerate() {
        let entry = scores.entry(slug.as_str()).or_insert_with(|| {
            order.push(slug.as_str());
            0.0
        });
        *entry += w_sem / (k + rank as f64);
    }
    for (rank, slug) in bm25_order.iter().enumerate() {
        let entry = scores.entry(slug.as_str()).or_insert_with(|| {
            order.push(slug.as_str());
            0.0
        });
        *entry += w_bm25 / (k + rank as f64);
    }

    let mut fused: Vec<&str> = order;
    fused.sort_by(|a, b| {
        let sa = scores[a];
        let sb = scores[b];
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.into_iter().map(|s| s.to_string()).collect()
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn chunk(index: usize, start: usize, end: usize, vector: Vec<f32>) -> ChunkVec {
        ChunkVec {
            index,
            start,
            end,
            vector,
        }
    }

    fn page(slug: &str, dim: u32, model: &str, hash: &str, chunks: Vec<ChunkVec>) -> PageEmbeddings {
        PageEmbeddings {
            slug: slug.to_string(),
            dim,
            model: model.to_string(),
            body_hash: hash.to_string(),
            chunks,
        }
    }

    // ── Roundtrip do `.bin` ──────────────────────────────────────────────────

    #[test]
    fn test_bin_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("minha-pagina.bin");
        let original = page(
            "minha-pagina",
            3,
            "text-embedding-3-small",
            "0123456789abcdef0123456789abcdef",
            vec![
                chunk(0, 0, 10, vec![0.1, 0.2, 0.3]),
                chunk(1, 8, 20, vec![-0.5, 0.0, 0.9]),
            ],
        );

        write_page_embeddings(&path, &original).unwrap();
        let read = read_page_embeddings(&path).unwrap();

        // slug vem do nome do arquivo, o resto do conteúdo do `.bin`.
        assert_eq!(read.slug, "minha-pagina");
        assert_eq!(read, original);
    }

    #[test]
    fn test_bin_rejects_bad_magic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("x.bin");
        std::fs::write(&path, b"NOPE bytes que nao sao um header valido").unwrap();
        let err = read_page_embeddings(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_bin_empty_chunks_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vazia.bin");
        let original = page("vazia", 4, "m", "ff", vec![]);
        write_page_embeddings(&path, &original).unwrap();
        let read = read_page_embeddings(&path).unwrap();
        assert_eq!(read, original);
        assert!(read.chunks.is_empty());
    }

    // ── Gates dim/model + hash ───────────────────────────────────────────────

    #[test]
    fn test_is_compatible_detects_divergence() {
        let store = VectorStore::new();
        store.upsert(page("p", 3, "model-a", "h", vec![chunk(0, 0, 1, vec![1.0, 0.0, 0.0])]));

        assert!(store.is_compatible("p", 3, "model-a"), "mesma dim+modelo → compatível");
        assert!(!store.is_compatible("p", 4, "model-a"), "dim divergente → stale");
        assert!(!store.is_compatible("p", 3, "model-b"), "modelo divergente → stale");
        assert!(!store.is_compatible("ausente", 3, "model-a"), "ausente → stale");
    }

    #[test]
    fn test_get_hash_and_remove() {
        let store = VectorStore::new();
        store.upsert(page("p", 2, "m", "deadbeef", vec![chunk(0, 0, 1, vec![1.0, 0.0])]));
        assert_eq!(store.get_hash("p").as_deref(), Some("deadbeef"));
        assert_eq!(store.get_hash("nope"), None);

        store.remove("p");
        assert_eq!(store.get_hash("p"), None);
        assert!(store.is_empty());
    }

    // ── Cosseno ──────────────────────────────────────────────────────────────

    #[test]
    fn test_cosine_known_cases() {
        // Idênticos → 1.0.
        assert!((cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-6);
        // Ortogonais → 0.0.
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        // Opostos → -1.0.
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Mesma direção, magnitudes diferentes → 1.0 (L2-normaliza).
        assert!((cosine(&[2.0, 0.0], &[5.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_edge_cases() {
        // Tamanhos diferentes → 0.0.
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        // Vazio → 0.0.
        assert_eq!(cosine(&[], &[]), 0.0);
        // Vetor nulo → 0.0 (evita divisão por zero).
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    // ── Busca semântica / max-pool ───────────────────────────────────────────

    #[test]
    fn test_semantic_search_max_pool_picks_best_chunk() {
        let store = VectorStore::new();
        // Página com dois chunks: o segundo é o mais próximo da query.
        store.upsert(page(
            "doc",
            2,
            "m",
            "h",
            vec![
                chunk(0, 0, 5, vec![1.0, 0.0]),   // ortogonal à query
                chunk(1, 5, 12, vec![0.0, 1.0]),  // idêntico à query
            ],
        ));

        let hits = store.semantic_search(&[0.0, 1.0], 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "doc");
        assert!((hits[0].score - 1.0).abs() < 1e-6, "score = melhor chunk");
        // O chunk vencedor é o índice 1 (offsets pro snippet).
        assert_eq!(hits[0].best_index, 1);
        assert_eq!(hits[0].best_start, 5);
        assert_eq!(hits[0].best_end, 12);
    }

    #[test]
    fn test_semantic_search_ranks_and_truncates() {
        let store = VectorStore::new();
        store.upsert(page("perto", 2, "m", "h", vec![chunk(0, 0, 1, vec![0.0, 1.0])]));
        store.upsert(page("meio", 2, "m", "h", vec![chunk(0, 0, 1, vec![0.7, 0.7])]));
        store.upsert(page("longe", 2, "m", "h", vec![chunk(0, 0, 1, vec![1.0, 0.0])]));

        let hits = store.semantic_search(&[0.0, 1.0], 2);
        assert_eq!(hits.len(), 2, "top_k trunca");
        assert_eq!(hits[0].slug, "perto");
        assert_eq!(hits[1].slug, "meio");
    }

    // ── RRF ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_rrf_both_rankings_beat_single() {
        // "a" aparece bem nos dois rankings; "b" só num; "c" só no outro.
        let bm25 = vec!["a".to_string(), "c".to_string()];
        let sem = vec!["a".to_string(), "b".to_string()];
        let fused = rrf_fuse(&bm25, &sem, RRF_K, W_SEM, W_BM25);
        assert_eq!(fused[0], "a", "presente nos dois rankings vence");
    }

    #[test]
    fn test_rrf_semantic_weight_breaks_tie() {
        // "s" lidera só o ranking semântico; "l" lidera só o BM25, mesmas posições.
        // Com peso pró-semântica (W_SEM > W_BM25), "s" deve vencer.
        let bm25 = vec!["l".to_string()];
        let sem = vec!["s".to_string()];
        let fused = rrf_fuse(&bm25, &sem, RRF_K, W_SEM, W_BM25);
        assert_eq!(fused[0], "s", "peso pró-semântica desempata a favor da semântica");
        assert_eq!(fused[1], "l");
    }

    #[test]
    fn test_rrf_no_duplicates() {
        let bm25 = vec!["a".to_string(), "b".to_string()];
        let sem = vec!["b".to_string(), "a".to_string()];
        let fused = rrf_fuse(&bm25, &sem, RRF_K, W_SEM, W_BM25);
        assert_eq!(fused.len(), 2, "slugs em ambos não duplicam");
        let set: std::collections::HashSet<_> = fused.iter().collect();
        assert_eq!(set.len(), 2);
    }

    // ── load_all ─────────────────────────────────────────────────────────────

    #[test]
    fn test_load_all_scans_dir() {
        let dir = TempDir::new().unwrap();
        let edir = dir.path();

        write_page_embeddings(
            &edir.join("um.bin"),
            &page("um", 2, "m", "h1", vec![chunk(0, 0, 1, vec![1.0, 0.0])]),
        )
        .unwrap();
        write_page_embeddings(
            &edir.join("dois.bin"),
            &page("dois", 2, "m", "h2", vec![chunk(0, 0, 1, vec![0.0, 1.0])]),
        )
        .unwrap();
        // Arquivo não-.bin é ignorado; .bin corrompido é pulado (não derruba).
        std::fs::write(edir.join("ruido.txt"), b"nao e bin").unwrap();
        std::fs::write(edir.join("corrompido.bin"), b"lixo").unwrap();

        let store = VectorStore::load_all(edir);
        assert_eq!(store.len(), 2, "só os .bin válidos entram");
        assert_eq!(store.get_hash("um").as_deref(), Some("h1"));
        assert_eq!(store.get_hash("dois").as_deref(), Some("h2"));
    }

    #[test]
    fn test_load_all_missing_dir_is_empty() {
        let dir = TempDir::new().unwrap();
        let store = VectorStore::load_all(&dir.path().join("nao-existe"));
        assert!(store.is_empty());
    }
}
