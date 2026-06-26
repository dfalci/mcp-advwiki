# AdvWiki — persistent project memory for AI coding agents

AdvWiki is a local MCP server that gives coding agents a durable, searchable memory for a software project.

It stores project knowledge as Markdown files, indexes them for full-text search (BM25, with an optional semantic layer), and exposes them to MCP clients as tools and resources. The goal is simple: when an AI assistant learns something important about your system, it should not disappear when the chat ends.

Use it for architecture notes, service descriptions, decisions, integration patterns, investigated bugs, runbooks, raw evidence, and project conventions.

AdvWiki is local-first:

- no external server;
- no embeddings required;
- no third-party API calls;
- plain Markdown on disk;
- optional Git versioning;
- compatible with Obsidian-style wikilinks.

---

## Why this exists

AI coding assistants are useful inside one session, but they usually forget the hard-won context from previous sessions.

That means you keep repeating things like:

- why a service uses a queue instead of HTTP;
- which feature flag breaks staging;
- what caused a race condition last week;
- which service owns authentication;
- what was decided during an architecture discussion;
- which workaround is safe and which one was rejected.

AdvWiki turns that project knowledge into a local, searchable wiki that agents can read before answering and update when you explicitly ask them to preserve something.

It is not meant to replace source code, logs, tests, or official documentation. It is a project memory layer that helps the agent recover context and avoid starting from zero.

---

## What you get

- **Searchable Markdown wiki** — pages live under `.advwiki/pages/`.
- **Hybrid search** — BM25 full-text (Tantivy) for exact technical terms, service names, errors, queues, endpoints, and config keys, plus an **optional semantic layer** that matches by meaning when enabled (see [Semantic search](#semantic-search-optional)).
- **MCP tools and resources** — agents can search, read, create, update, lint, and organize the wiki.
- **Raw evidence store** — logs, extracted files, specs, API snippets, and long pasted content can be stored separately from curated pages.
- **Frontmatter metadata** — pages can declare `type`, `project`, `status`, `tags`, `sources`, `related`, `confidence`, and `owner`.
- **Obsidian-compatible links** — use `[[slug]]` and `[[slug|Display text]]` inside page bodies.
- **Graph tools** — backlinks, orphans, related pages, link suggestions, and Mermaid graph output.
- **Reviewable edits** — agents can propose a diff before changing a page.
- **Traceable claims** — important statements can include source, confidence, and last verification date.
- **Optional Git autocommit** — keep wiki changes versioned in an isolated Git repository inside `.advwiki/`.

---

## Quick install

### 1. Install the binary

```bash
npm install -g mcp-advwiki@latest
```

Check that it works:

```bash
mcp-advwiki --help
```

### 2. Pick the project root

AdvWiki stores its files under the project root you pass with `--root`.

```bash
mcp-advwiki --root /path/to/your/project
```

If you omit `--root`, AdvWiki uses the current working directory of the process. For real projects, prefer passing `--root` explicitly so the MCP client always points to the right folder.

### 3. Install the bundled Claude skill

The repository ships with an `advwiki-memory` skill that teaches the agent when to search the wiki, when not to write, how to structure pages, and how to use claims and links.

```bash
mcp-advwiki --skill --root /path/to/your/project
```

This writes:

```text
/path/to/your/project/.claude/skills/advwiki-memory/skill.md
```

Re-run the same command after upgrading AdvWiki to refresh the skill.

### 4. Add AdvWiki to Claude Code

From your project directory:

```bash
claude mcp add --transport stdio advwiki -- mcp-advwiki --root "$(pwd)"
```

With Git autocommit enabled:

```bash
claude mcp add --transport stdio advwiki -- mcp-advwiki --root "$(pwd)" --autocommit
```

On Windows PowerShell:

```powershell
$project = (Get-Location).Path
mcp-advwiki --skill --root $project
claude mcp add --transport stdio advwiki -- mcp-advwiki --root $project --autocommit
```

Verify:

```bash
claude mcp list
```

Inside Claude Code, use:

```text
/mcp
```

### 5. Add AdvWiki to Claude Desktop

Edit `claude_desktop_config.json` and add:

```json
{
  "mcpServers": {
    "advwiki": {
      "type": "stdio",
      "command": "mcp-advwiki",
      "args": ["--root", "/path/to/your/project"]
    }
  }
}
```

With Git autocommit:

```json
{
  "mcpServers": {
    "advwiki": {
      "type": "stdio",
      "command": "mcp-advwiki",
      "args": ["--root", "/path/to/your/project", "--autocommit"]
    }
  }
}
```

Restart Claude Desktop after editing the config.

---

## First useful prompts

After installing, ask your agent something concrete:

```text
Search AdvWiki for what we already know about the authentication flow.
```

```text
Create a page documenting the queue-service architecture. Use frontmatter, claims, and links where useful.
```

```text
Record this decision in AdvWiki: we will use SQS for asynchronous invoice processing instead of synchronous HTTP, because retries and backpressure matter more than immediate response time.
```

```text
Run lint_wiki quick and tell me what needs cleanup.
```

```text
Show the wiki graph in Mermaid format.
```

```text
Propose an update to the payment integration page, but show me the diff before applying it.
```

---

## How AdvWiki thinks about knowledge

AdvWiki separates **raw evidence** from **curated knowledge**.

### Raw evidence

Use raw sources for content that should be preserved but not necessarily read as documentation:

- logs;
- stack traces;
- long pasted conversations;
- extracted file content;
- API responses;
- CSV, JSON, Markdown, or text dumps;
- investigation notes before they are cleaned up.

Raw sources live under:

```text
.advwiki/sources/
.advwiki/metadata/
rawindex.md
```

### Curated pages

Use wiki pages for durable, human-readable project knowledge:

- service overview;
- architecture decisions;
- integration contracts;
- runbooks;
- known bugs;
- deployment notes;
- project conventions;
- important flows;
- synthesized investigation results.

Curated pages live under:

```text
.advwiki/pages/
```

The best workflow is:

1. store raw evidence when needed;
2. summarize the durable part into one or more curated pages;
3. link curated pages back to raw evidence using `sources` or `## Claims`;
4. keep pages small, linked, and searchable.

---

## Recommended workflow

### Before answering project-specific questions

The agent should search AdvWiki when the user asks about a concrete project, service, module, endpoint, queue, table, deployment issue, bug, or prior decision.

Good searches are short and exact:

```text
queue-service retry policy
```

```text
gateway authentication jwt
```

```text
staging feature flag invoice
```

By default, search is BM25 — exact names, errors, and technical terms matter most. With the optional semantic layer enabled (see [Semantic search](#semantic-search-optional)), paraphrased and conceptual questions work too, but exact terms still help.

### When writing to the wiki

Agents should not write to AdvWiki just because they found something interesting. They should write only when the user explicitly asks to record, save, document, register, or update knowledge.

For small additive changes, `update_page` with `append` is usually fine.

To change one part of an existing page, pass `section` (an existing heading title) to `update_page`: `mode: overwrite` replaces that section's body, `mode: append` adds to its end. The server reconstructs the rest of the page byte-for-byte, so this is safer than a full `overwrite` for a localized edit.

For substantial changes, prefer:

1. `propose_page_update`;
2. show the diff;
3. ask for approval;
4. `apply_page_update`.

### After bulk changes

The navigable index page (`wiki://page/index`) is regenerated automatically by
the server whenever pages change, so there is no manual rebuild step. A quick
lint is still worth running:

```text
lint_wiki quick
```

Use graph tools to keep navigation healthy:

```text
orphans
link_suggestions
backlinks
wiki_graph
```

---

## Semantic search (optional)

By default, search is **BM25 only** — lexical, exact-term matching. AdvWiki can also add a **semantic** layer: pages are embedded into vectors and queries are matched by meaning, which recovers content even when you ask with different words than were originally written. This matters for "AI memory" use, where the agent that writes and the agent that searches rarely use the same vocabulary, and where most content is non-English.

Semantic search is **opt-in and purely additive**:

- it is **off** unless `DD_WIKI_OPENAI_APIKEY` is set — with it off, behavior is identical to today;
- when on, `query_wiki` becomes **hybrid**: BM25 and semantic results are fused, with semantic preferred;
- a page with no embedding yet still appears via BM25 — the semantic layer only ever adds recall, never removes it.

### How to enable

Set a single environment variable in the AdvWiki process — that is the trigger:

```bash
export DD_WIKI_OPENAI_APIKEY=sk-...
```

Embeddings are produced by an external **OpenAI-compatible** API (`POST /v1/embeddings`). There is no local model and no bundled ONNX runtime. The same contract works with OpenAI and with local servers (Ollama, LM Studio, TEI) — just point the base URL at them:

```bash
# Example: a local embedding server
export DD_WIKI_OPENAI_APIKEY=local            # any non-empty value to flip the gate
export DD_WIKI_OPENAI_BASEURL=http://localhost:11434/v1
export DD_WIKI_OPENAI_MODEL=nomic-embed-text
```

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `DD_WIKI_OPENAI_APIKEY` | _(unset)_ | **Gate**: any non-empty value enables semantic search. |
| `DD_WIKI_OPENAI_BASEURL` | `https://api.openai.com/v1` | Base URL of the OpenAI-compatible embeddings API. |
| `DD_WIKI_OPENAI_MODEL` | `text-embedding-3-small` | Embedding model to request. |
| `DD_WIKI_CHUNK_CHARS` | `2000` | Target chunk size, in characters, for splitting pages before embedding. |

The vector dimension is auto-detected from the API response and recorded with each page's embedding — there is no dimension variable to set.

### What gets embedded

- **Only curated pages** are embedded. Raw sources (`raw://source/...`) remain BM25-only.
- Embeddings are cached on disk under `.advwiki/embeddings/{slug}.bin` and are rebuildable, so they are git-ignored (like the Tantivy `index/`). They are gated by a hash of the page body, so a change that only touches `updated_at` does not re-embed.

### Cost note

The **first activation on an existing wiki embeds every page** through the API, which incurs one batch of embedding calls. After that, only new or changed pages are embedded. Enabling never blocks startup: AdvWiki serves BM25 immediately and populates embeddings in the background.

### Controlling and inspecting it

- `query_wiki` accepts an optional `mode` argument: `auto` (default — hybrid fusion), `bm25` (force lexical only), or `semantic` (prefer meaning, falling back to BM25 when semantic results are unavailable). When semantic search is off, `mode` has no effect.
- `lint_wiki` reports a **Semantic search** status line: whether it is enabled, the model in use, and how many pages have embeddings (`N/M pages`).

---

## Directory layout

AdvWiki creates and manages this structure under the selected root:

```text
<project-root>/
  .advwiki/
    pages/        # curated Markdown pages
    sources/      # raw source contents
    metadata/     # JSON metadata for raw sources
    proposals/    # reviewable page update proposals
    index/        # Tantivy index, rebuilt automatically
    embeddings/   # semantic vectors (only if semantic search is enabled), rebuildable
  .advwikilog.md  # operational log
  rawindex.md     # readable index of raw sources
```

The `.advwiki/index/` folder is generated. Do not edit it manually.

---

## Page format

Pages are plain Markdown. YAML frontmatter is optional but recommended.

```markdown
---
type: service
project: billing
status: active
tags:
  - backend
  - invoices
sources:
  - raw://source/session-2026-05-30-billing-investigation
related:
  - billing-integration-payments
  - decision-invoice-processing-async
confidence: high
owner: platform-team
---

# Billing Service

## Summary

One or two sentences explaining what this page records.

## Context

Why this knowledge matters.

## Details

Curated technical content.

## Decisions Made

- **What**: ...
- **Why**: ...
- **Rejected alternatives**: ...

## Points of Attention

Gotchas, risks, limitations, operational notes.

## Claims

- The billing service publishes invoice processing jobs asynchronously.
  - Source: [[decision-invoice-processing-async]]
  - Confidence: high
  - Last verified: 2026-05-30

- The retry timeout was confirmed from production logs.
  - Source: `raw://source/prod-log-2026-05-30`
  - Confidence: medium
  - Last verified: 2026-05-30

## See also

- [[billing-integration-payments]] — payment provider integration
- [[decision-invoice-processing-async]] — async processing decision
```

AdvWiki manages `created_at` and `updated_at` automatically on writes.

---

## Links and URIs

Inside page bodies, use Obsidian-compatible wikilinks:

```markdown
See [[queue-service-overview]] for the consumer flow.
See [[queue-service-endpoints|REST endpoints]] for API details.
```

Use `wiki://` and `raw://` as MCP-level resource identifiers:

| URI | Meaning |
|---|---|
| `wiki://page/{slug}` | Full wiki page |
| `wiki://page/index` | Generated navigable wiki index |
| `wiki://index` | Raw source index, same content as `rawindex.md` |
| `wiki://rawindex` | Alias for `wiki://index` |
| `wiki://log` | Operational log |
| `raw://source/{id}` | Raw source content |
| `raw://sourcemetadata/{id}` | Raw source metadata |

Do not use legacy `[Text](wiki://page/slug)` links in page bodies. New content should use `[[slug]]`.

---

## Tools at a glance

### Search and read

| Tool | Purpose |
|---|---|
| `query_wiki` | Search pages and optionally raw sources — BM25, or hybrid BM25+semantic when semantic search is enabled (`mode`: `auto`/`bm25`/`semantic`) |
| `read_knowledge_uri` | Read `wiki://...` or `raw://...` content |
| MCP resources | Passive resource listing and reading through the MCP client |

### Write and review

| Tool | Purpose |
|---|---|
| `update_page` | Create, append, or overwrite a page — or, with `section`, edit a single section |
| `set_page_metadata` | Edit a page's frontmatter (scalars and list fields) without resending the body |
| `propose_page_update` | Create a reviewable proposal with a unified diff (whole page, or a single `section`) |
| `apply_page_update` | Apply a proposal, with change conflict protection |
| `delete_page` | Remove a wiki page |

### Raw evidence

| Tool | Purpose |
|---|---|
| `ingest_source` | Store content from HTTP/HTTPS or local file path |
| `ingest_extracted_content` | Store already-extracted text as a raw source |
| `delete_raw_source` | Remove raw source content and metadata |

### Organization

| Tool | Purpose |
|---|---|
| `list_pages_by_type` | Find pages by frontmatter `type` |
| `list_pages_by_project` | Find pages by frontmatter `project` |
| `list_pages_by_tag` | Find pages by tag |
| `find_pages_without_sources` | Find pages without documented sources |
| `lint_wiki` | Run wiki quality checks |

### Graph and navigation

| Tool | Purpose |
|---|---|
| `wiki_graph` | Render graph summary, full adjacency, or Mermaid diagram |
| `backlinks` | List pages that point to a page |
| `orphans` | Find pages with no incoming links |
| `related_pages` | Show related pages and relationship type |
| `link_suggestions` | Suggest missing links based on content similarity |

### Claims

| Tool | Purpose |
|---|---|
| `find_claims` | List traceable claims |
| `find_claims_without_source` | Find claims without source metadata |
| `find_conflicting_claims` | Heuristic conflict-review candidates |
| `verify_claim` | Update a claim's last verification date |

---

## Obsidian compatibility

AdvWiki pages are just Markdown files under:

```text
.advwiki/pages/
```

You can open that folder as an Obsidian vault, or include it inside an existing vault. No plugin is required.

Both directions work:

- edit a page in Obsidian → AdvWiki watcher reindexes it;
- ask the agent to update a page → Obsidian sees the updated Markdown;
- links use `[[slug]]`, so the Obsidian graph and AdvWiki graph stay aligned.

Older AdvWiki pages that used `wiki://page/...` links are migrated automatically on startup. The migration is one-time, creates a backup, and is safe to re-run.

To preview migration without writing:

```bash
ADVWIKI_MIGRATION_DRYRUN=1 mcp-advwiki --root /path/to/project
```

---

## Git autocommit

Use `--autocommit` when you want AdvWiki to version wiki changes automatically.

```bash
mcp-advwiki --root /path/to/project --autocommit
```

AdvWiki initializes a separate Git repository inside:

```text
/path/to/project/.advwiki/.git/
```

This keeps wiki history isolated from the project repository.

It versions useful content such as:

```text
pages/
sources/
metadata/
```

It ignores rebuildable or transient content such as:

```text
index/
proposals/
migration backups
```

Commits are debounced and generated from domain events. If an upstream remote is configured, AdvWiki attempts a best-effort `git push`. Push failures are logged but do not block the server.

Example remote setup:

```bash
git -C /path/to/project/.advwiki remote add origin git@github.com:your-org/your-project-advwiki.git
git -C /path/to/project/.advwiki push -u origin main
```

---

## Multiple instances

AdvWiki supports multiple processes pointing at the same wiki root.

One instance becomes **primary** and owns Tantivy indexing. Other instances run as **secondary** readers. If the primary exits, a secondary can promote itself and take over indexing.

This matters when more than one MCP client or assistant is connected to the same project wiki.

---

## Build from source

```bash
git clone https://github.com/dfalci/mcp-advwiki.git
cd mcp-advwiki
cargo build --release
```

Run it:

```bash
./target/release/mcp-advwiki --root /path/to/project
```

Install the bundled skill from the local build:

```bash
./target/release/mcp-advwiki --skill --root /path/to/project
```

---

## Best practices

### Prefer specific pages

Avoid generic pages like:

```text
architecture
bugs
notes
decisions
```

Prefer specific slugs:

```text
orders-service-overview
orders-integration-payment-provider
decision-jwt-authentication
pattern-retry-with-backoff
billing-known-bugs
```

### Keep pages curated

Do not turn wiki pages into log dumps. Store long evidence as raw sources and summarize what matters in curated pages.

### Link aggressively, but usefully

Every important page should point to related services, decisions, runbooks, and integrations.

### Use claims only for load-bearing facts

Not every sentence needs to be a claim. Use claims for facts that may need verification later: limits, timeouts, retry behavior, queue semantics, security assumptions, external service behavior, and non-obvious invariants.

### Let the user control writes

The safest agent behavior is:

- search proactively;
- answer from recovered context;
- write only when the user explicitly asks;
- propose diffs for substantial edits.

---

## When not to use AdvWiki

AdvWiki is not the right storage for:

- secrets;
- credentials;
- full source code dumps;
- unstable scratch notes that should disappear;
- data that belongs in tests, code comments, or official docs;
- information the user did not ask to preserve.

---

## Origin

AdvWiki is inspired by Andrej Karpathy's idea of giving AI agents a searchable local wiki as long-term project memory.

This implementation packages that idea as a standalone MCP server for coding assistants.

---

## License

MIT.

