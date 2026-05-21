# AdvWiki — a local wiki with search, made for AIs

AdvWiki is an [MCP](https://modelcontextprotocol.io) server that puts a local wiki right on your AI agent's desk. It runs over `stdio` with JSON-RPC 2.0 and was thought out to run inside clients like openclaude, deepseek, codex cli and others...

Under the hood, it uses [Tantivy](https://github.com/quickwit-oss/tantivy) for full-text search with BM25 — the same algorithm that powers Elasticsearch and Lucene. Everything runs locally, no external server, no embedding, no third-party APIs.

Implementation from Karpathy's gist at https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f

---

## Why does this exist?

### The ephemeral memory problem

When you chat with an AI, it operates within a context window. Everything you sent in the last messages is right there... but when the session ends or the context gets full, that stuff vanishes. It doesn't matter how much the AI "learned" about your project in that chat: on the next one, it starts from zero.

AdvWiki solves this by keeping knowledge in Markdown files that survive any session. The AI doesn't need to remember... it just looks it up.

This is also different from searching in a project's local memory which tends to get huge and outdated. By using this kind of service, the documentation is alive, editable, and the search is fast and relevant.

### Microservices and scattered documentation

In multi-service architectures, system knowledge gets fragmented. Each service has its own repo, its own docs, its own logs. A new dev on the team or an AI trying to help needs to hunt for info in five different places before being able to answer a simple question.

AdvWiki serves as a single point of reference. You just toss documentation pages, processed logs, API specs, and decision notes into the wiki, and the BM25 search finds what matters in milliseconds. For the AI consuming the MCP, this means a question like _"which service handles authentication?"_ can be answered with a search + read, without anyone needing to open five repos.

### A second brain for the project

Think of AdvWiki as a persistence layer between you and the AI. Everything the AI discovers about the code, every architectural decision you guys discuss, every bug that was hard-diagnosed, you register in the wiki. Next chat, the AI looks it up and picks up right where it left off. It's not short-term memory nor a garbage dump of memory. It's accumulated knowledge.

---

## Getting Started

### What you need

- Node.js + npm if you want the easiest install path...
- Rust (edition 2024, stable toolchain) only if you want to build from source.
- Git only if you want to clone the repository.

### Install with npm (recommended)

If you're just trying to use AdvWiki in Claude Desktop or another MCP client, the easiest path is installing it globally with npm:

```bash
npm install -g mcp-advwiki@latest
```

That gives you the `mcp-advwiki` command in your terminal, so you can run it directly:

```bash
mcp-advwiki
```

If you want AdvWiki to use another folder as the project root, pass `--root`:

```bash
mcp-advwiki --root /path/to/project
```

If you omit `--root`, AdvWiki keeps the current behavior and uses the current working directory of the process.

### Build from source (optional)

If you prefer building everything yourself...

```bash
# Clone the repository
git clone https://github.com/dfalci/mcp-advwiki.git
cd mcp-advwiki

# Build in release mode (optimized binary)
cargo build --release

# The binary will be at target/release/mcp-advwiki
```

After building, you can run it straight with Cargo:

```bash
cargo run
```

Or use the compiled binary directly:

```bash
./target/release/mcp-advwiki
```

### Choosing the project root

By default, AdvWiki keeps the current behavior: it uses the **current working directory of the process** as the project root.

That selected root is where the server expects and/or creates:

- `.advwiki/`
- `.advwikilog.md`
- `rawindex.md`

If you want the server to initialize the wiki structure from another OS folder, start it with the optional `--root <path>` parameter:

```bash
mcp-advwiki --root /path/to/project

# or, if you're running from source with Cargo
cargo run -- --root /path/to/project

# or using the compiled binary
./target/release/mcp-advwiki --root /path/to/project
```

You can also use the inline form:

```bash
./target/release/mcp-advwiki --root=/path/to/project
```

If you omit the parameter, the behavior remains exactly the same as today.

---

## Frontmatter

Every wiki page can carry a YAML frontmatter block at the top — the same format used by Jekyll and Hugo:

```markdown
---
type: service
project: auth-service
status: active
tags:
  - backend
  - api
sources:
  - raw://source/abc123
related:
  - auth-adr-001
owner: alice
confidence: high
---

# Auth Service

Your content here.
```

The server manages `updated_at` and `created_at` automatically: every time you call `update_page` (or `propose_page_update`), `updated_at` is injected or refreshed (format `YYYY-MM-DD`). `created_at` is only written once, on the first save.

Pages without a frontmatter block are fully supported — the fields are all optional. The frontmatter is stripped before indexing, so BM25 search sees only the actual Markdown content.

### Supported fields

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Semantic type: `service`, `decision`, `pattern`, `runbook`, `bug`, `note`, `index`, ... |
| `project` | string | Which project this page belongs to |
| `status` | string | e.g. `active`, `draft`, `deprecated`, `accepted` |
| `created_at` | date | Set automatically on first write (YYYY-MM-DD) |
| `updated_at` | date | Updated automatically on every write (YYYY-MM-DD) |
| `confidence` | string | How reliable the content is: `high`, `medium`, `low` |
| `sources` | list | `raw://source/<id>` URIs that back this page |
| `related` | list | Slugs of related pages |
| `tags` | list | Free-form tags |
| `owner` | string | Person responsible for keeping it up to date |

---

## How it works

AdvWiki has four main pieces that talk to each other:

### The watcher — eyes on the filesystem

There's a module (`watcher.rs`) that monitors the wiki directories using the `notify` crate. Every time you create, edit, or remove a `.md` file inside `.advwiki/pages/`, the watcher notices and sends an event through the internal channel.

Same thing for _raw sources_ ... raw files you've indexed (logs, CSVs, API JSONs) that live in `.advwiki/sources/`.

### The search engine — BM25 on disk

The `search.rs` module maintains a Tantivy index in `.advwiki/index/`. Whenever a new page event arrives, the index gets updated. Tantivy tokenizes the text and keeps the search structures ready to go.

When the AI asks _"search for 'JWT authentication'"_, AdvWiki isn't doing a grep... it's running a search ranked by statistical relevance. BM25 weighs term frequency, rarity in the corpus, and document size. The result: the most relevant page pops up first.

### The MCP server — the bridge to the AI

The `mcp_server.rs` module implements the MCP protocol over stdin/stdout. It exposes:

- **Resources**: reading pages, lists, logs, metadata. The AI accesses it as if it were a logical filesystem (`wiki://page/home`, `raw://source/abc123`).
- **Tools**: actions the AI can trigger: search, create page, delete, validate index integrity, download external content.

Everything via JSON-RPC 2.0, just like the MCP spec says.

### The storage — everything in its right place

The `storage.rs` module manages the directory structure in `.advwiki/` under the selected project root:

```
.advwiki/
  pages/        - your Markdown pages ({slug}.md)
  sources/      - indexed raw content
  metadata/     - JSON metadata for each raw source
  proposals/    - pending/applied change proposals ({proposal_id}.json)
  index/        - Tantivy index (managed automatically)
```

In that same selected root you'll find `.advwikilog.md` (operational log) and `rawindex.md` (readable index of raw sources).

### Fully reactive

On startup, AdvWiki rebuilds the index by scanning everything that already exists on disk. After that, it keeps listening for changes. You edit a `.md` in your editor and the AI can already search for the new content in the very next message.

---

## Search as a RAG tool

RAG (_Retrieval-Augmented Generation_) is the pattern where the AI queries an external knowledge base before answering, instead of relying only on what's in the context window.

AdvWiki implements the "R" in RAG: the retrieval step. The `search_advwiki` tool takes a text query and returns the most relevant docs from the index. The AI then uses these docs as context to generate its answer.

In practice, the flow goes like:

1. You ask: _"what's the authentication flow in the gateway service?"_
2. The AI decides it needs extra context
3. Calls `search_advwiki` with the query `gateway authentication flow`
4. BM25 returns the 5 most relevant pages (with score and snippet)
5. The AI reads the full pages via `read_resource`
6. Answers based on real content, not on what it "remembers" or hallucinates

This is super handy when the project code is too big to fit in a system prompt or a simple memory. Instead of trying to cram 200 files into the prompt, you document the important parts in the wiki and let the search do the heavy lifting of filtering it all out.

---

## Obsidian compatibility

AdvWiki stores pages as plain Markdown files inside `.advwiki/pages/`. Point [Obsidian](https://obsidian.md) at that folder (or any vault that includes it) and **the same files become editable on both sides** — no plugin, no sync layer, no format translation.

### Wikilink syntax everywhere

Inline links use `[[slug]]` and `[[slug|Display text]]` — the same notation Obsidian uses natively. Backlinks, the graph view, and Obsidian's link suggestions all work out of the box. From the wiki side, `wiki_graph`, `backlinks`, `orphans`, `related_pages`, `link_suggestions`, and the lint's broken-link check follow the exact same links.

```markdown
Veja [[queue-service-overview]] para começar.
Tabela completa em [[queue-service-endpoints|os endpoints REST]].
```

### Bidirectional editing

You can edit a page in Obsidian and the server picks it up via the filesystem watcher (Tantivy reindexes within seconds). You can also have the AI edit via `update_page` / `propose_page_update` and Obsidian sees the new content immediately. Both directions are first-class.

### Automatic, one-time migration

Wikis created with previous AdvWiki versions used the legacy `[Text](wiki://page/slug)` form for inline links. The server migrates them to wikilink syntax **automatically on the next boot** — no skill action, no tool call required:

- Idempotent (gated by a `.advwiki/.schema-version` marker — never runs twice).
- Creates a full backup in `.advwiki/.backup-pre-wikilinks-{timestamp}/` before any write.
- Atomic per file (write-temp + rename); safe to interrupt.
- Logs a summary line to `.advwikilog.md` describing what changed.
- Preserves code blocks, inline code (outside Claims `Source:` fields), frontmatter, and `raw://` URIs untouched.
- Dry-run mode: set `ADVWIKI_MIGRATION_DRYRUN=1` to preview without writing.

The link parser is also lenient: even after migration, pages pasted from older backups or external sources keep working — both `[[slug]]` and `wiki://page/slug` are recognized as the same edge in the graph.

### What `wiki://` still means

`wiki://` is **not gone** — it remains the MCP protocol identifier scheme used by:

| Use | Example | Visible to Obsidian? |
|---|---|---|
| MCP resources URI / `read_knowledge_uri` arg | `wiki://page/home`, `wiki://log`, `wiki://index` | No |
| Tool params (`backlinks`, `verify_claim`, ...) | `slug: "wiki://page/home"` | No |
| Search index document key (Tantivy) | `wiki://page/home` | No |
| Inline link in a page body | ~~`[Home](wiki://page/home)`~~ → use `[[home]]` | **Yes** |

Only the last row affects what Obsidian renders. Everything else is JSON-RPC plumbing the AI sees and Obsidian never touches.

---

## Skills and the learning process

One of the coolest ideas about AdvWiki is using it together with a **Skill** — an instruction file that guides the AI's behavior.

Check the skills directory. We offer a baseline skill that teaches the AI how to use the wiki effectively. You can customize it or create your own.

### How it works

A Skill might contain a learning script. Something like:

1. Read `wiki://index` to understand what's documented
2. For each listed service, read the corresponding page
3. Build a mental map of the architecture
4. Compare it with `rawindex.md` to cross-reference with indexed source code

The AI follows this script on its first interaction with the project. The knowledge it gains gets recorded in the wiki itself (creating summary pages, diagrams, notes), so in the next session the process is incremental — it doesn't have to relearn everything, just look up what's already been registered.

---

## Setting up in Claude Desktop

If you installed AdvWiki with npm, this is the preferred setup for `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "advwiki": {
      "command": "mcp-advwiki",
      "args": ["--root", "/path/to/your/project"]
    }
  }
}
```

If you're running from a locally built binary instead, point `command` to that file path.

```json
{
  "mcpServers": {
    "advwiki": {
      "command": "/path/to/mcp-advwiki/target/release/mcp-advwiki",
      "args": ["--root", "/path/to/your/project"]
    }
  }
}
```

If you prefer the old behavior, keep `"args": []` and the server will continue using the process working directory as its base folder.

Restart openclaude or similar and the server naturally shows up as an available tool.

---

## Logical URIs

AdvWiki exposes content through a simple URI scheme:

| Scheme | URI | Returns |
|---------|-----|---------|
| `wiki://` | `wiki://list` | List of all pages (slugs) |
| `wiki://` | `wiki://page/{slug}` | Full contents of the page |
| `wiki://` | `wiki://index` | Main index (rawindex) |
| `wiki://` | `wiki://rawindex` | Readable raw index |
| `wiki://` | `wiki://log` | Operational log |
| `raw://` | `raw://sources` | List of available raw sources |
| `raw://` | `raw://source/{id}` | Raw contents of the raw source |
| `raw://` | `raw://sourcemetadata/{id}` | JSON metadata |

The URIs are accessible both as **resources** (passive reading) and via **tools** like `read_knowledge_uri`.

> These are **protocol-level identifiers** used between the AI and the server. For inline links inside page bodies use `[[slug]]` (Obsidian-compatible wikilink syntax) — see the [Obsidian compatibility](#obsidian-compatibility) section.

---

## Available tools

The AI has access to these MCP tools (names match what `tools/list` returns):

- **query_wiki** — full-text BM25 search. Args: `question` (required), `maxPages` (1–50, default 10), `includeRawReferences` (bool, default false; when false only wiki pages are returned).
- **update_page** — creates or updates a page. Args: `slug`, `mode` (`overwrite` | `append`), `content`; optional `rationale` is appended to the operational log.
- **propose_page_update** — proposes a page change *without writing it*. Stores a reviewable proposal under `.advwiki/proposals/<id>.json` and returns a unified diff between the current and proposed content. Args: `slug`, `content` (the full proposed Markdown), `reason`. Returns a `proposal_id` to be used with `apply_page_update`.
- **apply_page_update** — applies a proposal created by `propose_page_update`. Args: `proposalId`, optional `force` (default false). Before writing, it re-checks via an MD5 hash that the page has not changed since the proposal; on a mismatch it refuses unless `force` is set. The operation is recorded in the operational log.
- **delete_page** — removes a page by `slug`; optional `rationale` is logged.
- **ingest_source** — downloads external content (HTTP/HTTPS or local file path) and stores it as a raw source. Args: `sourceUri`, `sourceType`, optional `force` (default false). The `source_id` is a stable MD5 of `sourceUri`.
- **ingest_extracted_content** — saves already-extracted text as a raw source. Args: `logicalUri` (must be `raw://source/<id>`), `sourceType`, `title`, `content`, optional `force`.
- **delete_raw_source** — removes a raw source (content + metadata) and updates `rawindex.md`. Args: `sourceId`; optional `rationale` is logged.
- **lint_wiki** — wiki quality report. Args: `scope` (`quick` | `all`). `quick` checks: broken internal links (both `[[slug]]` and legacy `wiki://page/slug` pointing to missing pages), orphan pages (no page links to them), raw sources with no derived page, pages without frontmatter, pages over 50 KB, pages missing a "See also" section. `all` adds: stale pages (file not modified in 90+ days), decision pages (`decisao-*`, `decision-*`, `adr-*`) missing a rationale section (`## Rationale`, `## Justificativa`, etc.), and similar page pairs (Jaccard token similarity > 60% — duplicate/merge candidates).
- **list_pages_by_type** — lists pages whose frontmatter `type` field matches the given value. Args: `pageType` (e.g. `service`, `decision`).
- **list_pages_by_project** — lists pages whose frontmatter `project` field matches. Args: `project`.
- **list_pages_by_tag** — lists pages that contain the given tag in frontmatter. Args: `tag`.
- **find_pages_without_sources** — lists pages with no `sources` field in frontmatter (or with it empty) — candidates for linkage with raw sources. No args.
- **rebuild_wiki_index** — scans all pages, reads their frontmatter, and writes a navigable index to `wiki://page/index` grouped by `type` and `project`. Run this after bulk imports or reorganizations. No args.
- **wiki_graph** — renders the wiki link graph. Edges come from inline page links in either `[[slug]]` (Obsidian) or legacy `wiki://page/slug` form, and from the frontmatter `related` field. Args: optional `format` (`summary` — counts plus top hubs; `full` — adjacency list; `mermaid` — diagram; default `summary`).
- **backlinks** — lists pages that point to a given page. Args: `slug` (also accepts a `wiki://page/{slug}` URI).
- **orphans** — lists pages with no incoming links. Links from the generated `index` page are ignored, so the index does not mask real orphans. No args.
- **related_pages** — lists pages related to a given page, classifying each relationship as bidirectional, declared (frontmatter `related`), links-to, or linked-from. Args: `slug`.
- **link_suggestions** — suggests links between not-yet-connected pages, ranking by content similarity (Jaccard) plus boosts for shared `project` and tags. Args: optional `slug` (focus on one page; otherwise scans the whole wiki), `maxSuggestions` (default 10), `minSimilarity` (default 0.15).
- **find_claims** — lists traceable claims (the `## Claims` block) across the wiki, with each claim's text, source, confidence, and last-verified date. Args: optional `slug` (focus one page; otherwise scans the whole wiki).
- **find_claims_without_source** — lists claims with no `Source` field — statements with no documented origin. No args.
- **find_conflicting_claims** — heuristic triage: flags pairs of claims with overlapping vocabulary as conflict-review candidates. It does not detect contradiction. Args: optional `minSimilarity` (Jaccard, default 0.25).
- **verify_claim** — updates a claim's `Last verified` date, marking it as re-checked. Args: `slug`, `claimIndex` (1-based), optional `date` (default today).
- **read_knowledge_uri** — reads any logical URI (`wiki://page/{slug}`, `wiki://log`, `wiki://index`, `wiki://rawindex`, `raw://source/{id}`, `raw://sourcemetadata/{id}`). Args: `uri`.

Passive reads (page list, log, raw sources, metadata) are also available through MCP **resources** via `resources/list` and `resources/read` — no tool call required for plain reads.

---

## License

MIT.
