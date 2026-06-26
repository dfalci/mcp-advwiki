// ── Módulo de Embeddings (busca semântica, opt-in) ──────────────────────────
//
// Camada PURA de embeddings: configuração por env var, provider de embeddings
// (API OpenAI-compatible) e chunking estrutura-agnóstico do corpo das páginas.
//
// Este módulo NÃO se conecta a search/watcher/mcp_server — a fiação do ciclo de
// vida (worker, storage `.bin`, fusão híbrida na busca) é de cards posteriores.
//
// Responsabilidades:
//   - SemanticConfig         → gate (`DD_WIKI_OPENAI_APIKEY`) + config por env var
//   - EmbeddingProvider      → trait + `OpenAiEmbedder` (rede) + `FakeEmbedder` (testes)
//   - chunk_text/chunk_page  → quebra boundary-aware recursiva em offsets de byte

use serde::Deserialize;

// ── Configuração / gate ──────────────────────────────────────────────────────

/// Configuração da busca semântica. É derivada das env vars de prefixo
/// `DD_WIKI_`; a presença de uma `api_key` não vazia é o que liga a feature.
#[derive(Debug, Clone)]
pub struct SemanticConfig {
    /// Chave da API OpenAI-compatible (gatilho da feature).
    pub api_key: String,
    /// Base URL do endpoint (`{base_url}/embeddings`), sem barra final.
    pub base_url: String,
    /// Modelo de embedding (ex.: `text-embedding-3-small`).
    pub model: String,
    /// Alvo de tamanho de chunk, em bytes do corpo.
    pub chunk_chars: usize,
}

impl SemanticConfig {
    /// Construtor direto, usado pelos testes (sem tocar em env var, que é global
    /// ao processo). Normaliza a `base_url` removendo barras finais.
    pub fn from_values(api_key: String, base_url: String, model: String, chunk_chars: usize) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            api_key,
            base_url,
            model,
            chunk_chars: chunk_chars.max(1),
        }
    }

    /// Lê a config das env vars. Retorna `None` (feature desligada) quando
    /// `DD_WIKI_OPENAI_APIKEY` está ausente ou vazia. Defaults:
    ///   - `DD_WIKI_OPENAI_BASEURL` → `https://api.openai.com/v1`
    ///   - `DD_WIKI_OPENAI_MODEL`   → `text-embedding-3-small`
    ///   - `DD_WIKI_CHUNK_CHARS`    → `2000` (valor inválido cai para 2000)
    pub fn from_env() -> Option<SemanticConfig> {
        let api_key = std::env::var("DD_WIKI_OPENAI_APIKEY").ok()?;
        if api_key.trim().is_empty() {
            return None;
        }
        let base_url = std::env::var("DD_WIKI_OPENAI_BASEURL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("DD_WIKI_OPENAI_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        let chunk_chars = std::env::var("DD_WIKI_CHUNK_CHARS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(2000);
        Some(Self::from_values(api_key, base_url, model, chunk_chars))
    }

    /// Overlap entre chunks consecutivos — não é configurável, é 20% do alvo.
    pub fn overlap(&self) -> usize {
        self.chunk_chars / 5
    }
}

// ── Provider de embeddings ───────────────────────────────────────────────────

/// Erro de embedding, classificado para o worker (card posterior) decidir retry.
#[derive(Debug, Clone)]
pub enum EmbedError {
    /// Falha provavelmente temporária (rede, 408/429/5xx): vale retentar.
    Transient(String),
    /// Falha definitiva (4xx de request/credencial): não adianta retentar.
    Permanent(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Transient(m) => write!(f, "erro transitório de embedding: {m}"),
            EmbedError::Permanent(m) => write!(f, "erro permanente de embedding: {m}"),
        }
    }
}

impl std::error::Error for EmbedError {}

/// Future devolvido por [`EmbeddingProvider::embed`]. Boxed (em vez de `async
/// fn`) para a trait ser dyn-compatible.
pub type EmbedFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send + 'a>>;

/// Fonte de embeddings. Devolve UM vetor por input, NA MESMA ORDEM dos inputs.
///
/// O retorno é um future boxed (não `async fn`) para a trait ser
/// dyn-compatible: o ciclo de vida da indexação guarda o provider como
/// `Arc<dyn EmbeddingProvider>` (concreto em produção, fake nos testes).
pub trait EmbeddingProvider: Send + Sync {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbedFuture<'a>;
}

/// Provider real: bate numa API OpenAI-compatible (`POST {base_url}/embeddings`).
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAiEmbedder {
    /// Cria o provider a partir da config (clona os campos relevantes).
    pub fn new(cfg: &SemanticConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            api_key: cfg.api_key.clone(),
        }
    }
}

/// Resposta do endpoint `/embeddings` (apenas os campos usados).
#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
    index: usize,
}

impl EmbeddingProvider for OpenAiEmbedder {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbedFuture<'a> {
        Box::pin(async move {
            if inputs.is_empty() {
                return Ok(Vec::new());
            }

            let url = format!("{}/embeddings", self.base_url);
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&serde_json::json!({ "model": self.model, "input": inputs }))
                .send()
                .await
                .map_err(|e| EmbedError::Transient(format!("falha de rede: {e}")))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let snippet: String = body.chars().take(300).collect();
                let msg = format!("status {status}: {snippet}");
                // 408/429/5xx são temporários; demais 4xx são definitivos.
                let code = status.as_u16();
                if code == 408 || code == 429 || status.is_server_error() {
                    return Err(EmbedError::Transient(msg));
                }
                return Err(EmbedError::Permanent(msg));
            }

            let parsed: EmbeddingResponse = resp
                .json()
                .await
                .map_err(|e| EmbedError::Transient(format!("falha ao decodificar resposta: {e}")))?;

            // A API pode devolver os data fora de ordem — reordena por `index`.
            let mut data = parsed.data;
            data.sort_by_key(|d| d.index);
            Ok(data.into_iter().map(|d| d.embedding).collect())
        })
    }
}

// ── Chunker estrutura-agnóstico ──────────────────────────────────────────────

/// Um chunk do corpo, em offsets de BYTE (sempre em fronteira de char, então
/// `&body[start..end]` é sempre válido). `index` é a posição na sequência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub index: usize,
    pub start: usize,
    pub end: usize,
}

/// Quebra `body` em chunks boundary-aware de ~`target` bytes com `overlap` bytes
/// de sobreposição entre consecutivos. Determinístico. Acumula até ~`target` e
/// prefere terminar na ÚLTIMA fronteira natural <= target, nesta ordem:
/// `\n\n` → `\n` → fim de frase (`. `/`! `/`? `) → espaço. Sem fronteira numa
/// unidade maior que `target`, corta duro em `target` (em fronteira de char).
/// Corpo vazio ou só de espaços → `Vec` vazio.
pub fn chunk_text(body: &str, target: usize, overlap: usize) -> Vec<Chunk> {
    if body.trim().is_empty() {
        return Vec::new();
    }
    let target = target.max(1);
    let len = body.len();
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < len {
        let max_end = (start + target).min(len);
        let end = if max_end >= len {
            len
        } else {
            let mut hard_limit = floor_char_boundary(body, max_end);
            if hard_limit <= start {
                // alvo cai dentro de um único char multibyte: avança pra cabê-lo.
                hard_limit = ceil_char_boundary(body, start + 1);
            }
            let split = find_split(body, start, hard_limit);
            if split > start { split } else { hard_limit }
        };

        chunks.push(Chunk {
            index: chunks.len(),
            start,
            end,
        });

        if end >= len {
            break;
        }

        // Próximo chunk recua `overlap` a partir do fim, até fronteira de char.
        let mut next = floor_char_boundary(body, end.saturating_sub(overlap));
        if next <= start {
            // overlap >= tamanho do chunk: abre mão da sobreposição pra progredir.
            next = end;
        }
        start = next;
    }

    chunks
}

/// Quebra o corpo de uma PÁGINA. Aplica `frontmatter::strip_frontmatter` ANTES
/// e chunka o resultado — então os offsets dos chunks são relativos ao corpo JÁ
/// SEM frontmatter (a busca re-aplica `strip_frontmatter` e fatia pelos mesmos
/// offsets).
pub fn chunk_page(content: &str, cfg: &SemanticConfig) -> Vec<Chunk> {
    let body = crate::frontmatter::strip_frontmatter(content);
    chunk_text(body, cfg.chunk_chars, cfg.overlap())
}

/// Encontra o offset de corte em `[start+1, hard_limit]`, preferindo a última
/// fronteira natural. `hard_limit` precisa estar em fronteira de char.
fn find_split(body: &str, start: usize, hard_limit: usize) -> usize {
    let window = &body[start..hard_limit];

    if let Some(pos) = window.rfind("\n\n") {
        return start + pos + 2;
    }
    if let Some(pos) = window.rfind('\n') {
        return start + pos + 1;
    }
    if let Some(rel) = last_sentence_end(window) {
        return start + rel;
    }
    if let Some(pos) = window.rfind(' ') {
        return start + pos + 1;
    }
    // Sem fronteira: corte duro (hard_limit já é fronteira de char).
    hard_limit
}

/// Acha o fim da última frase (`. `/`! `/`? `) em `s`; devolve o offset logo
/// após o espaço. `.!?` e espaço são ASCII, então o offset é fronteira de char.
fn last_sentence_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 2 {
        return None;
    }
    for i in (0..b.len() - 1).rev() {
        if matches!(b[i], b'.' | b'!' | b'?') && b[i + 1] == b' ' {
            return Some(i + 2);
        }
    }
    None
}

/// Maior fronteira de char `<= i`.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

/// Menor fronteira de char `>= i`.
fn ceil_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

// ── FakeEmbedder (apenas testes, determinístico, sem rede) ───────────────────

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Provider falso para testes: vetor de dimensão fixa L2-normalizado derivado de
/// um histograma de bytes do texto. Texto idêntico → vetor idêntico; textos
/// parecidos têm cosseno alto. Conta chamadas e pode ser configurado pra falhar.
#[cfg(test)]
#[derive(Clone)]
pub struct FakeEmbedder {
    dim: usize,
    calls: Arc<AtomicUsize>,
    fail: Option<EmbedError>,
}

#[cfg(test)]
impl FakeEmbedder {
    /// Provider determinístico com dimensão 64.
    pub fn new() -> Self {
        Self {
            dim: 64,
            calls: Arc::new(AtomicUsize::new(0)),
            fail: None,
        }
    }

    /// Variante com dimensão customizada.
    pub fn with_dim(dim: usize) -> Self {
        Self {
            dim,
            ..Self::new()
        }
    }

    /// Variante que sempre devolve `err` (para testar retry/skip).
    pub fn failing(err: EmbedError) -> Self {
        Self {
            fail: Some(err),
            ..Self::new()
        }
    }

    /// Quantidade de chamadas a `embed` até agora.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for &b in text.as_bytes() {
            v[b as usize % self.dim] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[cfg(test)]
impl EmbeddingProvider for FakeEmbedder {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbedFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(inputs.iter().map(|t| self.embed_one(t)).collect())
        })
    }
}

// ── Testes ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(chunk_chars: usize) -> SemanticConfig {
        SemanticConfig::from_values(
            "k".to_string(),
            "https://api.openai.com/v1".to_string(),
            "text-embedding-3-small".to_string(),
            chunk_chars,
        )
    }

    // ── SemanticConfig ───────────────────────────────────────────────────────

    #[test]
    fn test_from_values_normalizes_base_url_and_overlap() {
        let c = SemanticConfig::from_values(
            "key".to_string(),
            "http://localhost:11434/v1/".to_string(),
            "m".to_string(),
            2000,
        );
        assert_eq!(c.base_url, "http://localhost:11434/v1");
        assert_eq!(c.overlap(), 400);
    }

    #[test]
    fn test_overlap_is_20_percent() {
        assert_eq!(cfg(1000).overlap(), 200);
        assert_eq!(cfg(2000).overlap(), 400);
    }

    #[test]
    fn test_from_env_gating() {
        // Único teste que mexe nessas env vars (globais ao processo).
        unsafe {
            std::env::remove_var("DD_WIKI_OPENAI_APIKEY");
            std::env::remove_var("DD_WIKI_OPENAI_BASEURL");
            std::env::remove_var("DD_WIKI_OPENAI_MODEL");
            std::env::remove_var("DD_WIKI_CHUNK_CHARS");
        }
        // Ausente → None.
        assert!(SemanticConfig::from_env().is_none());

        // Vazia → None.
        unsafe { std::env::set_var("DD_WIKI_OPENAI_APIKEY", "   ") };
        assert!(SemanticConfig::from_env().is_none());

        // Presente → Some com defaults.
        unsafe { std::env::set_var("DD_WIKI_OPENAI_APIKEY", "sk-test") };
        let c = SemanticConfig::from_env().expect("deveria ligar");
        assert_eq!(c.api_key, "sk-test");
        assert_eq!(c.base_url, "https://api.openai.com/v1");
        assert_eq!(c.model, "text-embedding-3-small");
        assert_eq!(c.chunk_chars, 2000);

        // Valor inválido em CHUNK_CHARS cai para 2000.
        unsafe { std::env::set_var("DD_WIKI_CHUNK_CHARS", "abc") };
        assert_eq!(SemanticConfig::from_env().unwrap().chunk_chars, 2000);

        unsafe {
            std::env::remove_var("DD_WIKI_OPENAI_APIKEY");
            std::env::remove_var("DD_WIKI_CHUNK_CHARS");
        }
    }

    // ── Chunker ──────────────────────────────────────────────────────────────

    /// Todo chunk deve ter offsets válidos e `&body[start..end]` deve funcionar.
    fn assert_valid(body: &str, chunks: &[Chunk]) {
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index, i, "index sequencial");
            assert!(c.start < c.end, "start<end em {c:?}");
            assert!(body.is_char_boundary(c.start), "start não é fronteira {c:?}");
            assert!(body.is_char_boundary(c.end), "end não é fronteira {c:?}");
            // Não deve dar panic:
            let _ = &body[c.start..c.end];
        }
        if !chunks.is_empty() {
            assert_eq!(chunks[0].start, 0, "primeiro chunk começa em 0");
            assert_eq!(chunks.last().unwrap().end, body.len(), "último cobre o fim");
        }
    }

    #[test]
    fn test_chunk_empty_body() {
        assert!(chunk_text("", 100, 20).is_empty());
        assert!(chunk_text("   \n\n  \t ", 100, 20).is_empty());
    }

    #[test]
    fn test_chunk_short_body_single_chunk() {
        let body = "Texto curto.";
        let chunks = chunk_text(body, 100, 20);
        assert_eq!(chunks.len(), 1);
        assert_eq!(&body[chunks[0].start..chunks[0].end], body);
    }

    #[test]
    fn test_chunk_prefers_paragraph_boundary() {
        let body = "Primeiro parágrafo aqui.\n\nSegundo parágrafo aqui também, mais longo.";
        // target força corte dentro do segundo parágrafo, mas deve preferir o \n\n.
        let chunks = chunk_text(body, 30, 6);
        assert_valid(body, &chunks);
        assert!(chunks.len() >= 2);
        // O primeiro chunk termina logo após o `\n\n`.
        assert!(
            body[..chunks[0].end].ends_with("\n\n"),
            "esperava corte no parágrafo, veio: {:?}",
            &body[chunks[0].start..chunks[0].end]
        );
    }

    #[test]
    fn test_chunk_overlap_between_consecutive() {
        let body = "palavra ".repeat(60); // ~480 bytes, sem fronteiras fortes
        let chunks = chunk_text(&body, 100, 20);
        assert_valid(&body, &chunks);
        assert!(chunks.len() >= 2);
        for w in chunks.windows(2) {
            // O próximo começa antes do fim do anterior (sobreposição).
            assert!(w[1].start < w[0].end, "sem overlap entre {:?} e {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn test_chunk_giant_unit_hard_cut() {
        // Uma "palavra" única bem maior que o target, sem nenhuma fronteira.
        let body = "x".repeat(500);
        let chunks = chunk_text(&body, 100, 20);
        assert_valid(&body, &chunks);
        assert!(chunks.len() > 1, "deveria cortar duro a unidade gigante");
        // Cada chunk (menos o último) tem ~target de tamanho.
        assert_eq!(chunks[0].end - chunks[0].start, 100);
    }

    #[test]
    fn test_chunk_preserves_crlf_in_slices() {
        let body = "linha um\r\nlinha dois\r\n\r\nparágrafo dois com mais texto aqui.";
        let chunks = chunk_text(body, 20, 4);
        assert_valid(body, &chunks);
        // As fatias preservam exatamente os bytes (incluindo \r\n).
        for c in &chunks {
            let slice = &body[c.start..c.end];
            assert_eq!(slice, &body[c.start..c.end]);
        }
        // Reconstrução do início bate com o corpo original.
        assert!(body.starts_with(&body[chunks[0].start..chunks[0].end]));
    }

    #[test]
    fn test_chunk_multibyte_char_boundaries() {
        // Acentos (2 bytes em UTF-8) não podem ser cortados no meio.
        let body = "ação coração maçã ".repeat(40);
        let chunks = chunk_text(&body, 50, 10);
        assert_valid(&body, &chunks); // assert_valid já valida is_char_boundary
        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_chunk_slice_returns_expected_text() {
        let body = "Alpha beta. Gamma delta epsilon. Zeta eta theta.";
        let chunks = chunk_text(body, 20, 4);
        assert_valid(body, &chunks);
        // Concatenar removendo overlap reconstrói o corpo.
        let mut rebuilt = String::new();
        let mut cursor = 0;
        for c in &chunks {
            let from = c.start.max(cursor);
            rebuilt.push_str(&body[from..c.end]);
            cursor = c.end;
        }
        assert_eq!(rebuilt, body);
    }

    #[test]
    fn test_chunk_page_strips_frontmatter() {
        let content = "---\ntype: note\nproject: x\n---\nCorpo da página sem o YAML.";
        let chunks = chunk_page(content, &cfg(2000));
        assert_eq!(chunks.len(), 1);
        let body = crate::frontmatter::strip_frontmatter(content);
        // Offsets relativos ao corpo SEM frontmatter.
        assert_eq!(&body[chunks[0].start..chunks[0].end], body);
        assert!(!body.contains("type: note"));
    }

    #[test]
    fn test_chunk_deterministic() {
        let body = "Texto repetível para checar determinismo. ".repeat(20);
        let a = chunk_text(&body, 80, 16);
        let b = chunk_text(&body, 80, 16);
        assert_eq!(a, b);
    }

    // ── FakeEmbedder ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fake_embedder_deterministic_and_dim() {
        let fake = FakeEmbedder::new();
        let out = fake
            .embed(&["mesmo texto".to_string(), "mesmo texto".to_string()])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 64);
        assert_eq!(out[0], out[1], "texto idêntico → vetor idêntico");
        // L2-normalizado: norma ~1.
        let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn test_fake_embedder_similar_texts_high_cosine() {
        let fake = FakeEmbedder::new();
        let out = fake
            .embed(&[
                "o gato dorme no sofá".to_string(),
                "o gato dorme no sofá macio".to_string(),
                "zzz qqq www kkk".to_string(),
            ])
            .await
            .unwrap();
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let similar = cos(&out[0], &out[1]);
        let different = cos(&out[0], &out[2]);
        assert!(similar > different, "{similar} deveria > {different}");
    }

    #[tokio::test]
    async fn test_fake_embedder_counts_calls() {
        let fake = FakeEmbedder::new();
        assert_eq!(fake.call_count(), 0);
        let _ = fake.embed(&["a".to_string()]).await.unwrap();
        let _ = fake.embed(&["b".to_string()]).await.unwrap();
        assert_eq!(fake.call_count(), 2);
    }

    #[tokio::test]
    async fn test_fake_embedder_failing_variant() {
        let fake = FakeEmbedder::failing(EmbedError::Transient("falha".to_string()));
        let err = fake.embed(&["x".to_string()]).await.unwrap_err();
        assert!(matches!(err, EmbedError::Transient(_)));
    }

    #[tokio::test]
    async fn test_fake_embedder_custom_dim() {
        let fake = FakeEmbedder::with_dim(16);
        let out = fake.embed(&["abc".to_string()]).await.unwrap();
        assert_eq!(out[0].len(), 16);
    }
}
