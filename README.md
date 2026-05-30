# AdvWiki — persistent project memory for AI coding assistants

AdvWiki is an [MCP](https://modelcontextprotocol.io) server that gives an AI coding assistant a file-backed, searchable knowledge base for a software project. It runs over `stdio` with JSON-RPC 2.0 and works with any MCP client (Claude, Codex CLI, and others).

The problem it targets is specific to working with an AI on a codebase: the assistant loses everything it learned the moment the session ends. The cause of a race condition you spent an afternoon diagnosing, why service A talks to service B over a queue instead of HTTP, which config flag breaks staging — none of it carries over. Next session you explain it again. AdvWiki keeps that knowledge in Markdown files on disk and exposes full-text search over them, so the assistant retrieves what's already known instead of being re-told.

Search uses [Tantivy](https://github.com/quickwit-oss/tantivy) (BM25, the ranking model behind Lucene and Elasticsearch). Everything runs locally — no external server, no embeddings, no third-party APIs.

---

## Why does this exist?

### Context windows don't persist

An AI assistant operates inside a context window. What you sent in the last messages is available; once the session ends or the window fills up, it's gone. Whatever the assistant worked out about the project during that session doesn't carry into the next one — it starts from zero.

AdvWiki keeps the knowledge in Markdown files that outlive the session. The assistant doesn't need to remember; it queries the wiki.

This is also distinct from a single growing memory file, which tends to bloat and go stale. Here the content is editable Markdown, and retrieval is a ranked search rather than dumping the whole file into the prompt.

### Knowledge scattered across services

In a multi-service codebase, the information needed to answer one question is spread out: each service has its own repo, its own docs, its own logs. A new developer — or an assistant trying to help — has to look in several places before answering something basic.

AdvWiki acts as one reference point. You add documentation pages, processed logs, API specs, and decision notes; BM25 search ranks them on query. A question like _"which service handles authentication?"_ becomes a search plus a read, instead of opening five repos.

### Accumulated, not just remembered

AdvWiki sits as a persistence layer between you and the assistant. What the assistant figures out about the code, the architectural decisions made during a session, a bug that was hard to track down — those get written to the wiki. The next session reads them back. It isn't short-term context and it isn't an undifferentiated dump; it's knowledge that accumulates and stays queryable.

For the workflow this is meant to support — the assistant reading the index, searching for relevant context, doing the work, then recording what it learned — see [Skills and the learning process](#skills-and-the-learning-process).

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

### Versioning the wiki with git (`--autocommit`)

Pass `--autocommit` to keep the wiki content under git version control:

```bash
mcp-advwiki --root /path/to/project --autocommit
```

When enabled, AdvWiki manages a git repository **rooted at `<root>/.advwiki/`**
(fully isolated from any project repo that may contain the wiki):

- On first run it does `git init` and writes a pre-configured `.gitignore`
  that versions the useful content (`pages/`, `sources/`, `metadata/`) and
  ignores rebuildable/transient artifacts (`index/`, `proposals/`, migration
  backups).
- Each batch of changes is auto-committed (debounced ~3s) with a message
  derived from the operations, e.g. `wiki: update queue-service, delete old`.
- After each commit it runs a best-effort `git push` to the current branch's
  **upstream, if one is configured** — set up the remote once yourself
  (`git -C <root>/.advwiki remote add ...` + `git push -u ...`). Without an
  upstream, the push is skipped; network/auth failures only log and never
  block the wiki.
- Only the primary instance commits (matching the primary/secondary indexing
  roles). Commits are unsigned (machine-generated).

### Running inside claude

To use AdvWiki as a tool inside Claude, you first need to install the bundled skill (see the [Skills](#skills-and-the-learning-process) section below). 

The next step is to add the server to your `claude_desktop_config.json`: 

```bash
cd <your project folder>
mcp-advwiki --skill
claude mcp add mcp-advwiki -- mcp-advwiki
```

Now you're good to go

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

### The watcher — filesystem events

There's a module (`watcher.rs`) that monitors the wiki directories using the `notify` crate. Every time you create, edit, or remove a `.md` file inside `.advwiki/pages/`, the watcher notices and sends an event through the internal channel.

Same thing for _raw sources_ ... raw files you've indexed (logs, CSVs, API JSONs) that live in `.advwiki/sources/`.

### The search engine — BM25 on disk

The `search.rs` module maintains a Tantivy index in `.advwiki/index/`. Whenever a new page event arrives, the index gets updated. Tantivy tokenizes the text and keeps the search structures ready to go.

When the AI asks _"search for 'JWT authentication'"_, AdvWiki isn't doing a grep — it's running a search ranked by statistical relevance. BM25 weighs term frequency, rarity in the corpus, and document length, so the most relevant page ranks first.

### The MCP server — the bridge to the AI

The `mcp_server.rs` module implements the MCP protocol over stdin/stdout. It exposes:

- **Resources**: reading pages, lists, logs, metadata. The AI accesses it as if it were a logical filesystem (`wiki://page/home`, `raw://source/abc123`).
- **Tools**: actions the AI can trigger: search, create page, delete, validate index integrity, download external content.

Everything via JSON-RPC 2.0, just like the MCP spec says.

### The storage — directory layout

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

AdvWiki implements the "R" in RAG: the retrieval step. The `query_wiki` tool takes a text query and returns the most relevant docs from the index. The AI then uses these docs as context to generate its answer.

In practice, the flow goes like:

1. You ask: _"what's the authentication flow in the gateway service?"_
2. The AI decides it needs extra context
3. Calls `query_wiki` with the query `gateway authentication flow`
4. BM25 returns the 5 most relevant pages (with score and snippet)
5. The AI reads the full pages via `read_resource`
6. Answers based on real content, not on what it "remembers" or hallucinates

This matters when the project is too big to fit in a system prompt or a single memory file. Instead of pushing 200 files into the prompt, you document the parts that matter in the wiki and let search narrow it down at query time.

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

AdvWiki is meant to be used together with a **Skill** — an instruction file that guides how the AI uses the wiki: when to search before answering, when to write back what it learned, how to structure pages.

The repository ships a baseline skill (`advwiki-memory`) that covers this. You can customize it or write your own.

### Installing the bundled skill

The `advwiki-memory` skill ships embedded inside the binary. To drop it into a project, run:

```bash
# installs the skill in the current directory
mcp-advwiki --skill

# or pick the target project explicitly
mcp-advwiki --skill --root /path/to/project
```

The command creates `<root>/.claude/skills/advwiki-memory/skill.md` (creating parent folders if needed) and exits — it does not start the MCP server. `<root>` defaults to the current working directory; pass `--root` to target a different project.

Re-running `--skill` overwrites the file, so this is also how you refresh the skill after upgrading the binary.

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
| `wiki://` | `wiki://page/{slug}` | Full contents of the page |
| `wiki://` | `wiki://page/index` | Navigable wiki index generated by `rebuild_wiki_index` (grouped by `type` and `project`) |
| `wiki://` | `wiki://index` | Raw sources index — same content as `rawindex.md` (lines `source_id \| path \| extracted_at`) |
| `wiki://` | `wiki://rawindex` | Alias for `wiki://index` |
| `wiki://` | `wiki://log` | Operational log |
| `raw://` | `raw://source/{id}` | Raw contents of the raw source |
| `raw://` | `raw://sourcemetadata/{id}` | JSON metadata |

The URIs are accessible both as **resources** (passive reading) and via **tools** like `read_knowledge_uri`. To enumerate pages or raw sources, use `resources/list` or the `list_pages_by_*` tools — there is no `wiki://list` or `raw://sources` URI.

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

## Origin

The original idea comes from Andrej Karpathy's [gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) on giving an AI a searchable wiki as memory. AdvWiki is an implementation of that idea as a standalone MCP server.

---

## License

MIT.
