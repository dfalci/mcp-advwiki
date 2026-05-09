# AdvWiki MCP Server

Servidor [Model Context Protocol (MCP)](https://modelcontextprotocol.io) que expõe uma Wiki local com busca full-text via [Tantivy](https://github.com/quickwit-oss/tantivy) (BM25).

Projetado para ser consumido por clientes MCP como Claude Desktop, funcionando sobre `stdio` com JSON-RPC 2.0.

---

## Arquitetura

```
┌─────────────────────────────────────────────────────────┐
│                     main.rs (orquestrador)              │
│  tracing → stderr    │    Arc<WikiFileManager>          │
│                      │    Arc<WikiSearchEngine>         │
└────────┬─────────────┴──────────────┬───────────────────┘
         │                            │
    ┌────▼────────┐          ┌────────▼─────────┐
    │  watcher.rs │          │   mcp_server.rs  │
    │  (notify)   │          │  (JSON-RPC 2.0)  │
    │             │          │                  │
    │ WikiEvent ──┼──mpsc───►│  resources/list  │
    │ PageCreated │  reativo │  resources/read  │
    │ PageUpdated │          │  tools/list      │
    │ PageDeleted │          │  tools/call      │
    │ RawSource*  │          │                  │
    └────┬────────┘          └────────┬─────────┘
         │                             │
    ┌────▼────────┐          ┌─────────▼──────────┐
    │ storage.rs  │◄─────────│   search.rs        │
    │ (tokio::fs) │  lê do   │   (Tantivy BM25)   │
    │             │  disco   │                    │
    │ .advwiki/   │          │ index_document()   │
    │  pages/     │          │ delete_document()  │
    │  sources/   │          │ search()           │
    │  metadata/  │          │ index_bulk()       │
    └─────────────┘          └────────────────────┘
```

### Fluxo de reatividade

1. **`watcher.rs`** — Monitora `.advwiki/` recursivamente com a crate `notify`. Traduz eventos brutos do sistema de arquivos em `WikiEvent` (PageCreated, PageUpdated, etc.) e despacha via `tokio::sync::mpsc`.

2. **`main.rs`** — Uma task `tokio::spawn` consome os eventos, lê o conteúdo do disco via `WikiFileManager` e atualiza o índice Tantivy.

3. **`search.rs`** — Mantém o índice BM25. Quando uma página é criada no disco, o índice é atualizado automaticamente em milissegundos.

---

## URIs Lógicas (RFC `esquema://path`)

| Esquema   | Formato                              | Descrição                    |
|-----------|--------------------------------------|------------------------------|
| `wiki://` | `wiki://log`                         | Log operacional              |
| `wiki://` | `wiki://index`                       | Índice principal (rawindex)  |
| `wiki://` | `wiki://page/{slug}`                 | Página da Wiki               |
| `wiki://` | `wiki://list`                        | Lista de slugs               |
| `wiki://` | `wiki://rawindex`                    | Índice de raw sources        |
| `raw://`  | `raw://sources`                      | Lista de source IDs          |
| `raw://`  | `raw://source/{source_id}`           | Conteúdo bruto da source     |
| `raw://`  | `raw://sourcemetadata/{source_id}`   | Metadados JSON da source     |

---