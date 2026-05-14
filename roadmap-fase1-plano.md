# Plano detalhado — Fase 1 do roadmap

Este documento detalha como implementar a **Fase 1 — Fundação da wiki** descrita em `roadmap.md` (itens 1 a 4). O objetivo é deixar claro o que será construído, em que ordem, em quais arquivos, com quais contratos e como será testado, antes de qualquer mudança de código.

Os itens da Fase 1 são:

1. `index.md` navegável (separado de `rawindex.md`).
2. YAML frontmatter nas páginas.
3. `lint_wiki` reforçado.
4. Grafo de links e backlinks.

---

## 0. Estado atual relevante

Resumo do que já existe e que será extendido (referências `arquivo:linha`):

- Armazenamento em `src/storage.rs`: pastas `.advwiki/pages/`, `.advwiki/sources/`, `.advwiki/metadata/`, mais `.advwikilog.md` e `rawindex.md` na raiz. Páginas são Markdown puro — **não há frontmatter, nem `index.md`, nem parser de links**.
- Schema do Tantivy em `src/search.rs:104` indexa apenas `uri`, `title`, `content`, `kind`, `last_modified`. Nada de metadados estruturados.
- Watcher em `src/watcher.rs` já emite `PageCreated/Updated/Deleted` e `IndexChanged`/`LogChanged` — usaremos esses eventos para invalidar caches/índices auxiliares.
- Tools MCP atuais registradas em `src/mcp_server.rs:436-573`: `query_wiki`, `update_page`, `ingest_source`, `ingest_extracted_content`, `lint_wiki` (mínimo, em `mcp_server.rs:863-925`), `read_knowledge_uri`.

Conclusão: a base (I/O, índice, eventos) é sólida; toda a Fase 1 é aditiva.

---

## 1. Ordem de implementação e dependências

Embora o roadmap liste 1 → 4, a ordem técnica ótima é diferente. Frontmatter (item 2) e parser de links (parte do item 4) são **primitivas** que os outros itens consomem. Implementar primeiro evita retrabalho.

```
Sprint A — Primitivas (sem novas tools)
   1. Módulo `frontmatter` (parser/serializer + tipos)
   2. Módulo `wikilink`  (parser de [[slug]] e links Markdown)
   3. Extensão do schema Tantivy com campos derivados de frontmatter

Sprint B — Index navegável
   4. Tools: read_wiki_index, rebuild_wiki_index, update_wiki_index
   5. Geração automática (manual + assistida) do index.md

Sprint C — Grafo de links e backlinks
   6. Estrutura LinkGraph in-memory + invalidação por evento
   7. Tools: backlinks, orphans, wiki_graph, related_pages

Sprint D — Lint reforçado
   8. Checks novos no lint_wiki: scope quick vs full
   9. Relatório Markdown estruturado

Sprint E — Migração e DX
  10. Comando/CLI auxiliar para retroinjetar frontmatter mínimo em páginas existentes
  11. Documentação + entradas no README
```

Cada sprint termina com testes verdes e tools registradas; pode ser entregue/commitada de forma independente.

---

## 2. Sprint A — Primitivas

### 2.1 Módulo `frontmatter`

**Novo arquivo:** `src/frontmatter.rs` (~200-300 LoC + testes).

Responsabilidades:

- Detectar bloco YAML delimitado por `---` no topo de uma página.
- Parsear para um struct fortemente tipado, com defaults.
- Serializar de volta preservando ordem dos campos.
- Função `split(content) -> (Option<Frontmatter>, body: &str)`.
- Função `join(fm, body) -> String`.

**Dependência nova no `Cargo.toml`:**

```toml
serde_yaml = "0.9"   # ou "yaml-rust2" se quisermos zero-dep extra
```

`serde_yaml` está em modo de manutenção mas é mais que suficiente; alternativa é `yaml-rust2`. Decidir no início do sprint (ver §7 "Decisões em aberto").

**Tipo principal:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Frontmatter {
    pub r#type: Option<PageType>,        // service | decision | pattern | bug | runbook | integration | overview | note
    pub project: Option<String>,
    pub status: Option<PageStatus>,      // active | draft | deprecated | stale
    pub created_at: Option<chrono::NaiveDate>,
    pub updated_at: Option<chrono::NaiveDate>,
    pub confidence: Option<Confidence>,  // low | medium | high
    pub sources: Vec<String>,            // wiki:// ou raw:// URIs
    pub related: Vec<String>,            // slugs ou URIs
    pub tags: Vec<String>,
    pub owner: Option<String>,
    pub code_refs: Vec<CodeRef>,         // reservado para Fase 3 (item 10), mas o campo já entra no schema
}
```

`PageType`, `PageStatus`, `Confidence` viram enums com `#[serde(rename_all = "snake_case")]`. Valores desconhecidos resultam em erro de parsing, exceto quando o caller pediu modo permissivo (ver `lint_wiki`).

**Política em caso de erro de parsing:**

- `Storage::read_page` continua devolvendo o conteúdo bruto mesmo se o frontmatter estiver inválido (não quebra leitura).
- Há uma API separada `Storage::read_page_parsed(slug) -> (Result<Frontmatter, FrontmatterError>, String)` para o lint e para o índice.

**Testes** (em `#[cfg(test)] mod tests`):

- página sem `---`: retorna `None`, body inalterado.
- página com bloco YAML válido: todos os campos populados.
- chave desconhecida: erro com posição.
- enum inválido (`type: foo`): erro.
- datas inválidas: erro.
- round-trip: `join(split(x)) == x` modulo whitespace canonical.

### 2.2 Módulo `wikilink`

**Novo arquivo:** `src/wikilink.rs` (~150-250 LoC + testes).

Responsabilidades:

- Parser para `[[slug]]` e `[[slug|texto]]` no corpo Markdown.
- Parser para links Markdown que apontam para outras páginas: `[texto](slug)` ou `[texto](wiki://page/slug)` ou `[texto](./outra-pagina.md)`.
- Normalização de slug (mesma regra do `Storage::validate_slug`).
- Função `extract_links(body) -> Vec<LinkRef>` onde:

```rust
pub struct LinkRef {
    pub target: String,         // slug normalizado
    pub original: String,       // texto bruto encontrado
    pub kind: LinkKind,         // WikiStyle | Markdown
    pub byte_offset: usize,     // útil para lint apontar a posição
}
```

**Pegadinhas a tratar nos testes:**

- Ignorar `[[...]]` dentro de blocos de código (``` ``` `` e indented).
- Ignorar links HTTP externos (`[x](https://...)`).
- Ignorar âncoras (`#secao`) e considerar `slug#secao` como target `slug`.
- Aceitar `wiki://page/slug` mas extrair só o slug.

Reaproveitamos esta função tanto no grafo (Sprint C) quanto no lint (Sprint D).

### 2.3 Extensão do schema Tantivy

**Arquivo:** `src/search.rs:104` (função que monta o schema).

Adicionar campos **stored e indexed como STRING facetable**:

| Campo | Tipo | Origem |
|---|---|---|
| `fm_type` | STRING | `frontmatter.type` |
| `fm_project` | STRING | `frontmatter.project` |
| `fm_status` | STRING | `frontmatter.status` |
| `fm_tags` | STRING (multi) | `frontmatter.tags` |
| `fm_updated_at` | I64 | `frontmatter.updated_at` em epoch |
| `body_only` | TEXT | corpo sem o bloco frontmatter (para evitar que YAML polua BM25) |

`query_wiki` continua usando `title` + `content` por padrão, mas:

- Se a query incluir `project:xxx` ou `type:xxx` o parser do Tantivy já filtra (Tantivy aceita sintaxe `field:value` nativa). Documentar isso na descrição do tool.
- Novo parâmetro opcional `filters: { project, type, status, tags }` em `query_wiki` (não-breaking; default `{}`).

**Migração:** ao iniciar, se o schema gravado não tiver esses campos, deletar e recriar o índice (a wiki não tem migração formal; rebuild a partir dos arquivos é barato). Adicionar versão do schema em metadado para detectar.

**Testes** em `src/search.rs`:

- Indexar página com frontmatter: campos `fm_*` consultáveis.
- Indexar página sem frontmatter: campos `fm_*` ausentes mas `content` ainda indexado.
- Filtro por `project:foo` retorna só páginas do projeto foo.
- Body excluído do frontmatter: termo presente apenas no YAML não casa em busca padrão.

---

## 3. Sprint B — Index navegável

### 3.1 Arquivo `index.md`

**Localização:** `.advwiki/pages/index.md` (mesma pasta das outras páginas — assim aparece nas tools genéricas e é versionável junto). **Não** confundir com `rawindex.md` na raiz, que continua sendo o índice de raw sources.

**Estrutura canônica** (gerada por `rebuild_wiki_index`):

```markdown
---
type: overview
status: active
updated_at: 2026-05-14
---

# Wiki Index

<!-- BEGIN: auto-generated sections. Conteúdo entre tags é regerado. -->
<!-- BEGIN: projects -->
## Projects

- [[project-1-visao-geral]] — overview of `project-1`
<!-- END: projects -->

<!-- BEGIN: services -->
## Services

- [[microservice-1-integracao-externa]] — ...
<!-- END: services -->

<!-- BEGIN: decisions -->
## Cross-Cutting Decisions

- [[decisao-vetores-por-escopo]]
<!-- END: decisions -->

<!-- BEGIN: patterns -->
## Patterns

- [[padrao-tool-calling-estrito]]
<!-- END: patterns -->

<!-- END: auto-generated sections. -->

## Manual sections

(Conteúdo fora das tags BEGIN/END é preservado intocado pelo `rebuild_wiki_index`.)
```

Os marcadores `<!-- BEGIN: ... -->` e `<!-- END: ... -->` permitem **regerar só as seções automáticas sem destruir conteúdo manual**. É um pattern conhecido (gen-from-tags) e simples de implementar.

**Agrupamento padrão** das páginas é por `frontmatter.type`:

| Seção do índice | Critério |
|---|---|
| `## Projects` | `type == "overview"` |
| `## Services` | `type == "service"` ou `type == "integration"` |
| `## Cross-Cutting Decisions` | `type == "decision"` |
| `## Patterns` | `type == "pattern"` |
| `## Runbooks` | `type == "runbook"` |
| `## Other` | sem `type` ou tipos não cobertos (apenas em modo `full`) |

Ordenação dentro da seção: alfabética por slug. Pode-se permitir override via campo `frontmatter.index_weight` futuramente, mas fica fora da Fase 1.

### 3.2 Novas tools

**`read_wiki_index`** — atalho para `read_knowledge_uri("wiki://page/index")`, com schema dedicado e descrição clara para o agente. Retorna Markdown bruto + lista parseada de entradas.

**`rebuild_wiki_index`** — varre `.advwiki/pages/*.md`, lê o frontmatter de cada uma, e regenera as seções automáticas de `index.md`. Cria o arquivo se não existir.

Schema:

```json
{
  "type": "object",
  "properties": {
    "dryRun": { "type": "boolean", "default": false,
                "description": "Se true, retorna o diff proposto sem gravar." },
    "preserveManualSections": { "type": "boolean", "default": true }
  }
}
```

Saída: relatório Markdown com (a) número de páginas agrupadas por seção, (b) páginas que ficaram em `## Other` (sinaliza falta de `type`), (c) diff resumido contra a versão anterior.

**`update_wiki_index`** — append/edição manual em uma seção específica (ex: adicionar nota acima de uma entrada gerada). Funciona apenas **fora** dos blocos `BEGIN/END`. Schema:

```json
{
  "type": "object",
  "properties": {
    "section": { "type": "string",
                 "description": "Nome do header (ex: 'Manual sections')" },
    "content": { "type": "string" },
    "mode":    { "type": "string", "enum": ["append", "replace"] }
  },
  "required": ["section", "content", "mode"]
}
```

### 3.3 Onde tudo isso vive

- Lógica de geração em novo módulo `src/wiki_index.rs` (~250 LoC).
- Registro das tools em `src/mcp_server.rs:436-573` (mantendo o padrão atual).
- Handlers em `mcp_server.rs` próximos a `tool_lint_wiki` (linha ~863) para manter convivência.

### 3.4 Eventos do watcher

Em `src/watcher.rs`: o watcher já notifica `PageCreated/Updated/Deleted`. **Não** vamos regerar o índice automaticamente em cada save (gera ruído e conflito quando o agente está escrevendo várias páginas em sequência). Em vez disso, o `rebuild_wiki_index` fica explícito; o lint avisa quando o índice está desatualizado (ver §4.2).

---

## 4. Sprint C — Grafo de links e backlinks

### 4.1 Estrutura in-memory

**Novo módulo:** `src/link_graph.rs` (~200-350 LoC).

```rust
pub struct LinkGraph {
    /// page slug -> set de slugs para os quais ela aponta
    out_edges: HashMap<String, HashSet<String>>,
    /// page slug -> set de slugs que apontam para ela
    in_edges:  HashMap<String, HashSet<String>>,
    /// links cujo target não existe (broken)
    broken:    HashMap<String, Vec<LinkRef>>,
}

impl LinkGraph {
    pub async fn build(storage: &Storage) -> anyhow::Result<Self> { ... }
    pub fn backlinks(&self, slug: &str) -> Vec<&str> { ... }
    pub fn orphans(&self) -> Vec<&str> { ... }       // sem in_edges e não-hub
    pub fn hubs(&self, top_n: usize) -> Vec<&str> { ... }
    pub fn broken_links(&self) -> impl Iterator<Item = (&str, &LinkRef)> { ... }
}
```

Build varre todas as páginas, parseia frontmatter (para considerar `related: []` como aresta também), e parseia o corpo com `wikilink::extract_links`. É O(N) no total de páginas + links.

**Invalidação:** o `McpServer` mantém um `RwLock<LinkGraph>`. Subscreve eventos do watcher e rebuilda em mudanças de página. Rebuild completo é barato para wikis típicas (<10k páginas, <1s). Otimização incremental fica para depois.

### 4.2 Novas tools

| Tool | Input | Output |
|---|---|---|
| `backlinks` | `{ slug \| uri }` | Lista de páginas que apontam para o slug, com snippet do contexto. |
| `orphans` | `{}` | Páginas sem backlinks (excluindo `index.md`). |
| `wiki_graph` | `{ format: "json" \| "mermaid", project?: string }` | Grafo completo ou filtrado por projeto. |
| `related_pages` | `{ slug, limit?: int }` | Frontmatter.related ∪ vizinhos no grafo. |

`link_suggestions` (mencionado no roadmap) **fica para a Fase 2** — depende de análise mais semântica e cabe melhor junto da ingestão curada.

### 4.3 Testes

Em `src/link_graph.rs`:

- Grafo de 3 páginas (A→B, B→C): `backlinks(B) == [A]`, `orphans == [A]` (se A não recebe links).
- Link quebrado: aparece em `broken_links`, **não** entra como aresta normal.
- Link em bloco de código: ignorado.
- Frontmatter.related: vira aresta.
- Rebuild após `PageDeleted`: arestas removidas.

---

## 5. Sprint D — Lint reforçado

### 5.1 Escopos

O tool atual aceita `scope: "all" | "quick"`. Vamos manter os mesmos valores, mas redefinir o conteúdo:

**`quick`** (deve rodar em <100ms em wiki média):

- Páginas sem frontmatter.
- Frontmatter com campos obrigatórios faltando (`type`).
- Links quebrados (target inexistente).
- Páginas órfãs (sem backlinks).
- Páginas sem atualização há >180 dias (com base em `updated_at` ou mtime do arquivo, conforme o que existir).
- Páginas sem seção "See also" (heurística simples: nenhuma menção a links wiki na página).
- Raw sources sem página derivada (cross-check com `rawindex.md` + frontmatter.sources).
- Index desatualizado (compara páginas presentes em `index.md` com páginas no disco).

**`full`** (`quick` + abaixo):

- Possíveis contradições: páginas com `frontmatter.status: deprecated` ainda referenciadas.
- Conceitos recorrentes sem página própria: termos com TF-IDF alto em >=3 páginas e que **não** correspondem a um slug existente. Lista os top-N candidatos.
- Decisões sem rationale: `type: decision` cujo corpo não tem seção `## Context` ou `## Decision` (case-insensitive).
- Integrações citadas sem página dedicada: páginas que mencionam `microservice-X` no corpo mas não têm link para `microservice-X-*`.
- Páginas grandes demais: >800 linhas ou >40kB sugerem split.
- Baixa confiança: páginas com `confidence: low`.
- Páginas stale por mudança de código: reservado para Fase 3 (mensagem "not implemented yet").

### 5.2 Formato do relatório

Markdown com headers fixos por categoria, para que o consumidor (agente ou humano) consiga grep. Cada item tem link `wiki://page/slug` clicável. Exemplo abreviado:

```markdown
# Wiki Lint Report

Scope: full
Generated: 2026-05-14T12:34:56Z
Pages scanned: 87
Issues found: 12

## Broken links

- [`project-1-arquitetura`](wiki://page/project-1-arquitetura) → `project-1-deploy` (linha 42)

## Orphan pages

- [`bug-cdn-spa-access-denied`](wiki://page/bug-cdn-spa-access-denied)

## Decisions without rationale

- [`decisao-cache-de-sessao`](wiki://page/decisao-cache-de-sessao) — sem seção `## Context` ou `## Decision`

## Index drift

- 3 páginas no disco não estão em `index.md`: `padrao-foo`, `runbook-bar`, `decisao-baz`
- 1 página em `index.md` não existe mais: `pagina-removida-x`
```

### 5.3 Implementação

Substituir o corpo de `tool_lint_wiki` em `src/mcp_server.rs:863-925`. A lógica vira:

```rust
let report = LintEngine::new(storage, &link_graph).run(scope).await?;
Ok(report.to_markdown())
```

Com `src/lint.rs` novo (~300-500 LoC). Cada check vira função separada para poder testar isolado e desabilitar individualmente no futuro.

**Saída adicional:** além do Markdown, devolver no `_meta` do tool result um JSON estruturado com contagens por categoria. Útil para automação (CI etc.).

### 5.4 Testes

Em `src/lint.rs`:

- Fixture com wiki mínima cobrindo cada check.
- Snapshot test do Markdown gerado (com timestamp normalizado).
- Performance: rodar `quick` em wiki de 1000 páginas geradas no tempfile, asserir <500ms.

---

## 6. Sprint E — Migração e DX

### 6.1 Retroinjeção de frontmatter

Subcomando CLI **`mcp-advwiki migrate-frontmatter [--dry-run]`** (em `src/main.rs`):

- Para cada página em `.advwiki/pages/*.md` que **não** tem frontmatter, inserir bloco mínimo:

```yaml
---
type: note
status: draft
created_at: <data do file mtime>
updated_at: <data do file mtime>
---
```

- Em `--dry-run`, só lista o que seria mudado.
- Faz backup `.advwiki/pages/.migrate-backup/<timestamp>/` antes de gravar.

Sem isso, todo o ecossistema (lint, index, grafo) começa cuspindo warnings em wikis pré-existentes. Pequeno custo, grande ROI.

### 6.2 Documentação

Atualizar `README.md` com:

- Nova seção "Page metadata (frontmatter)" com exemplo.
- Nova seção "Navigable index" explicando `index.md` vs `rawindex.md`.
- Tabela das novas tools.
- Nota sobre o comando `migrate-frontmatter` para upgrades.

Sem novas páginas Markdown além disso (o README já cobre o público-alvo).

---

## 7. Decisões em aberto

Itens que precisam ser resolvidos no início do Sprint A (ou, no máximo, antes do Sprint B):

1. **Biblioteca YAML**: `serde_yaml` (maintenance mode, mas ubíquo) vs `yaml-rust2` (mais moderno, deserialização menos ergonômica). **Recomendação: `serde_yaml`** — o suporte a `serde` direto economiza muito código e o "maintenance mode" não significa abandono.
2. **Onde mora `index.md`**: `.advwiki/pages/index.md` (proposto) vs `.advwiki/index.md` na pasta pai. **Recomendação: `.advwiki/pages/index.md`** porque entra naturalmente no Tantivy e nas tools existentes; o slug "index" passa a ser reservado.
3. **Convivência com `rawindex.md`**: manter na raiz como hoje (compatibilidade) e documentar a diferença, ou mover para `.advwiki/rawindex.md`? **Recomendação: manter** — qualquer mudança aqui é breaking.
4. **Migração automática vs manual** ao detectar schema antigo do Tantivy: rebuild silencioso na primeira boot vs exigir flag. **Recomendação: rebuild silencioso** — o índice é derivado, não há perda de dados.
5. **Atualização automática do `index.md`** após cada `update_page` — fazer ou não? **Recomendação: não fazer** na Fase 1; em vez disso, lint avisa drift. Reduz acoplamento e evita conflito durante escrita em rajada por agentes.

---

## 8. Riscos e mitigação

| Risco | Mitigação |
|---|---|
| Frontmatter inválido quebra leitura de página existente | `read_page` continua devolvendo conteúdo bruto; parsing estrito só no caminho de lint/index. |
| Rebuild do índice Tantivy lento em wikis grandes | Já paralelizável (Tantivy é multi-thread); benchmark no Sprint A para confirmar. |
| `[[wikilinks]]` em código fence detectados como links | Cobrir com testes; usar parser linear simples com state machine (code-fence-aware), sem markdown completo. |
| Conflito entre escrita manual e regeneração automática do `index.md` | Tags `BEGIN/END` delimitam zona auto; conteúdo fora é preservado. |
| Aumento de superfície da API MCP confunde o agente | Descrições claras de tool + mencionar no README qual tool usar para cada tipo de pergunta. |

---

## 9. Definição de pronto (Fase 1)

A Fase 1 é considerada concluída quando:

- [ ] Todas as 4 funcionalidades têm testes unitários verdes no `cargo test`.
- [ ] `cargo clippy --all-targets -- -D warnings` limpo.
- [ ] `index.md` existe automaticamente após `rebuild_wiki_index` numa wiki vazia.
- [ ] Lint em uma wiki de exemplo (montada como fixture) emite relatório não-trivial e estável (snapshot test).
- [ ] `query_wiki` aceita filtro por `project` e `type` com testes.
- [ ] `backlinks` retorna lista correta em wiki com pelo menos 5 páginas linkadas.
- [ ] README documenta frontmatter, `index.md` e as novas tools.
- [ ] Comando `migrate-frontmatter --dry-run` funciona em uma wiki sem frontmatter e reporta corretamente.
- [ ] Nenhuma regressão nas tools existentes (suite de testes atual continua verde).

---

## 10. Resumo do esforço

Estimativa grosseira de LoC e tempo (referência, não compromisso):

| Sprint | LoC novo | LoC modificado | Esforço |
|---|---|---|---|
| A — primitivas | 400-600 | 100 | 1-2 dias |
| B — index | 250-400 | 80 | 1 dia |
| C — grafo | 300-500 | 50 | 1-2 dias |
| D — lint | 400-600 | 60 | 1-2 dias |
| E — DX/docs | 150-250 | 200 | 0.5-1 dia |
| **Total** | **~1.5-2.4k** | **~500** | **5-8 dias** |

Não envolve mudanças disruptivas no que existe; é quase tudo aditivo. As únicas modificações invasivas são (a) o schema do Tantivy (mitigada por rebuild) e (b) a função `tool_lint_wiki` (substituída inteira, mas o tool e seu contrato externo continuam compatíveis).

---

## Próximos passos imediatos

1. Decidir os itens de §7 (biblioteca YAML e localização do `index.md`).
2. Abrir o Sprint A: criar `src/frontmatter.rs` e `src/wikilink.rs` com testes, sem ainda expor tools.
3. Estender o schema do Tantivy e a indexação.
4. Só então partir para Sprint B (tools de index), que é o primeiro entregável visível ao usuário/agente.
