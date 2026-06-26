# Changelog

All notable changes to `mcp-advwiki` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.5] - 2026-06-26

### Changed

- **Documentation: search is no longer described as BM25-only.** The README
  intro, the feature list, the search guidance, and the crate description used
  to present BM25/Tantivy as the only search backend; they now state that
  AdvWiki does **hybrid** search — BM25 full-text plus an optional semantic
  layer (matched by meaning, preferred via RRF fusion when enabled). No behavior
  change: semantic search remains opt-in via `DD_WIKI_OPENAI_APIKEY` and purely
  additive.
- **Roadmap**: item #11 ("optional hybrid search") marked as implemented, and a
  new item #14 added — generating embeddings **locally by default** (e.g.
  `fastembed`/ONNX, in-process) so semantic search no longer requires an
  external API, with the OpenAI-compatible provider kept as an optional
  alternative.

## [0.2.4] - 2026-06-26

### Changed

- **TLS via rustls**: enabled the `rustls-tls` feature on `reqwest` so the
  HTTPS client used by the embeddings provider ships a vendored, pure-Rust TLS
  stack instead of depending on the platform's native TLS / OpenSSL at build
  time — simpler, more portable static builds.

## [0.2.3] - 2026-06-26

### Added

- **Semantic search (optional, opt-in).** A semantic layer that coexists with
  the BM25 engine and, when enabled, is **preferred** through hybrid fusion. It
  turns on only when `DD_WIKI_OPENAI_APIKEY` is set; otherwise everything is a
  no-op and behavior is byte-for-byte identical to before.
  - **Embeddings via an OpenAI-compatible API** (`POST /v1/embeddings`, batched)
    — no local model, no ONNX, no binary bloat (`reqwest` was already a
    dependency). The same contract covers OpenAI and local servers
    (Ollama/LM Studio/TEI) by pointing `DD_WIKI_OPENAI_BASEURL` at them.
    Configured by `DD_WIKI_*` env vars: `DD_WIKI_OPENAI_APIKEY` (gate),
    `DD_WIKI_OPENAI_BASEURL` (default `https://api.openai.com/v1`),
    `DD_WIKI_OPENAI_MODEL` (default `text-embedding-3-small`),
    `DD_WIKI_CHUNK_CHARS` (default `2000`). The embedding dimension is
    auto-detected from the returned vector.
  - **Structure-agnostic, boundary-aware chunking** (recursive `\n\n` → `\n` →
    sentence → word, hard cut only as a last resort), 2000 chars with 20%
    overlap; only **pages** are embedded — raw sources stay BM25-only.
  - **On-disk vector store** at `.advwiki/embeddings/{slug}.bin` (f32 LE; magic
    `ADVE`, format version, dim, model name, MD5 of the page body, per-chunk
    offsets + vectors). It is rebuildable, so it is git-ignored like the Tantivy
    `index/`. The body-hash header gates re-embedding: a save that only touches
    `updated_at` is **not** re-embedded (no wasted API calls).
  - **Hybrid ranking via RRF**: per-page score is the max cosine over its chunks
    (max-pool); BM25 and semantic rankings are fused with Reciprocal Rank Fusion
    (`k=60`, weights semantic `2.0` / BM25 `1.0`). `query_wiki` gains an optional
    `mode` argument — `auto` (default, hybrid), `bm25` (force lexical), or
    `semantic` (prefer meaning, falling back to BM25 when unavailable).
  - **Indexing lifecycle**: only the **primary** instance embeds, through a
    dedicated `mpsc` worker (concurrency 4) mirroring the git committer. The boot
    scan and the file watcher enqueue slugs; transient API errors retry with
    backoff (3 attempts), permanent ones are logged and skipped (the page stays
    BM25-only). Boot never blocks — AdvWiki serves BM25 immediately and populates
    embeddings in the background.
  - **Degradation is additive, never subtractive**: a page with no embedding
    still appears via BM25; a query that fails to embed falls back to BM25; a
    `.bin` with a divergent model/dimension is treated as stale and re-embedded.
  - **`lint_wiki`** now reports a **Semantic search** status line: enabled/disabled,
    the model in use, and embedding coverage (`N/M pages`).
  - Testability: an `EmbeddingProvider` trait with a real `OpenAiEmbedder` and a
    deterministic `FakeEmbedder` keeps the unit/integration tests network-free.

## [0.2.2] - 2026-06-01

### Added

- **Section-scoped editing** for `update_page` and `propose_page_update`, via a
  new optional `section` argument — no new tools. When set to an existing
  heading title (ATX, e.g. `"Details"` or `"## Details"`), the operation edits
  only that section and the server reconstructs the rest of the page
  byte-for-byte, instead of forcing the agent to resend the whole document
  (which risked silently dropping content in an `overwrite`).
  - `update_page`: with `section`, `mode: overwrite` replaces the section body
    and `mode: append` adds to the end of it. Without `section`, behavior is
    unchanged.
  - `propose_page_update`: with `section`, `content` is just the new section
    body; the returned unified diff is small and focused, and applies via
    `apply_page_update` as usual (replace-only).
  - Boundaries respect heading **level**: a `## X` includes its `### Y`
    subsections and ends at the next heading of level `<=`. Headings inside
    fenced code blocks (` ``` ` / `~~~`) and the YAML frontmatter are ignored,
    and the line terminator (CRLF/LF) is preserved — so editing a section never
    rewrites unrelated lines. Unknown or ambiguous section names error out
    without writing (the not-found message lists the available sections).
    Sections are ATX headings (`#`) only; setext (`===`/`---`) headings are not
    recognized as section boundaries.
- **Automatic navigable-index regeneration**: the `wiki://page/index` page
  (grouped by `type` and `project`) is now regenerated automatically by the
  primary instance — once on boot and then debounced whenever pages change —
  instead of relying on a manual tool call. The debounce coalesces a burst of
  changes (bulk import, migration, `git checkout`) into a single regeneration
  rather than one per file. The regeneration writes the `index` page, which the
  watcher reports back as a change event; that self-event is filtered out
  (`event_dirties_index` returns `false` for the `index` slug), so it never
  schedules another regeneration — no loop. Only the primary instance
  regenerates (it owns the event loop), and a regeneration failure is logged
  without retrying tightly.
- **`set_page_metadata` tool**: edits a page's YAML frontmatter without
  resending the body. `set` defines scalar fields (`type`, `project`, `status`,
  `confidence`, `owner`, …); `add`/`remove` add or remove items from list fields
  (`tags`, `related`, `sources`) without duplicating. The frontmatter is parsed
  as a generic YAML mapping rather than the typed struct, so **every** field is
  preserved on write — including unknown/custom keys (e.g. Obsidian's
  `aliases`) that the typed parser would drop. The page must already exist;
  `created_at`/`updated_at` stay server-managed and are rejected if named.

### Removed

- **`rebuild_wiki_index` tool**: removed from the MCP surface. The navigable
  index page it produced is now kept up to date automatically (see above), so
  the manual command is redundant. Its generation logic moved to a reusable
  `index_page::generate` function.

## [0.2.1] - 2026-05-30

### Added

- **`--autocommit` CLI flag**: versions the wiki content in a git repository
  rooted at `<root>/.advwiki/`, isolated from any project repo containing the
  wiki. On first run it does `git init` and writes a pre-configured
  `.gitignore` (versioning `pages/`, `sources/`, `metadata/`; ignoring the
  Tantivy `index/`, `proposals/`, and migration backups). Each batch of
  changes is auto-committed (debounced, hooked into the watcher's domain
  events so git/index writes never trigger a commit) with a message derived
  from the operations, followed by a best-effort `git push` to the current
  branch's upstream when configured. Only the primary instance commits;
  commits are unsigned.

### Fixed

- **Dates on save**: `update_page`/`propose_page_update`/`verify_claim` now
  always record `created_at`/`updated_at` — a minimal frontmatter block is
  injected when the content has none (previously the dates were silently
  dropped).
- **Slug normalization**: `update_page`, `delete_page`, and
  `propose_page_update` now accept the `wiki://page/{slug}` form like the
  other slug-taking tools.
- **Watcher renames**: single-path rename events (`RenameMode::From`/`To`) are
  classified as delete/create instead of update, preventing stale entries in
  the search index.
- **Migration**: the bare-link converter no longer rewrites `wiki://` inside
  inline-code spans.
- **Search snippets**: truncation now counts characters (not bytes) and only
  appends `...` when it actually truncates.
- **Raw index**: `|` and newlines in `original_path` are sanitized so a single
  entry per line is preserved on round-trip.

## [0.2.0] - 2026-05-23

### Changed

- **`advwiki-memory` skill**: removed Portuguese terms from the embedded
  skill's suggestions. The suggested context-recovery format header,
  the example trigger phrases for writing to the wiki, and the search
  variants guidance are now fully in English. The note about claim field
  labels was rewritten to direct the agent to use only `Source`,
  `Confidence`, and `Last verified` — the localized aliases remain
  accepted server-side for legacy content but are no longer suggested as
  authoring options.

## [0.1.9] - 2026-05-23

### Added

- **`--skill` CLI flag**: installs the bundled `advwiki-memory` skill into the
  project. The skill content is embedded in the binary at compile time
  (`include_str!`), so a single invocation deploys the up-to-date version
  without downloading anything:
  - Writes `<root>/.claude/skills/advwiki-memory/skill.md`, creating any
    missing parent directories. `<root>` is `--root` when provided, otherwise
    the current working directory.
  - Overwrites the file if it already exists — re-running `--skill` is the
    canonical way to refresh the skill after upgrading the binary.
  - Action-and-exit (like `--help`): does not start the MCP server.
  - Both `--skill` and `-skill` are accepted.
- **Multiple instances on the same wiki root** (primary/secondary roles).
  Until now Tantivy's exclusive cross-process writer lock prevented running a
  second `mcp-advwiki` against the same `.advwiki/` (the second process would
  fail to boot). The server now embraces concurrent instances:
  - On startup, the engine tries to acquire the Tantivy `IndexWriter` with a
    short retry sequence (100ms → 250ms → 500ms) to absorb near-simultaneous
    starts. The instance that wins becomes **primary** and owns indexing.
  - Instances that fail to acquire the lock enter **secondary** mode:
    `index_document` / `delete_document` / `index_bulk` / `clear` become
    no-ops (the primary will index the same files via its own watcher), and
    `IndexReader` uses `ReloadPolicy::OnCommitWithDelay` so secondaries see
    the primary's commits.
  - Secondaries run a background failover loop and try to promote themselves
    to primary every 5 seconds. When the current primary drops (process
    exits, crashes, etc.) the next polling secondary acquires the writer and
    transparently takes over indexing without restarting anything else.
  - The role is logged at startup (`role = "primary"` / `role = "secondary"`)
    and again on every promotion event.

### Fixed

- **README accuracy**: aligned the README with the actual MCP surface exposed
  by the server after a full audit of every Rust source file.
  - The "Search as a RAG tool" section called the search tool `search_advwiki`,
    but the tool registered in `tools/list` is `query_wiki`. Renamed both
    references.
  - The "Logical URIs" table listed `wiki://list` and `raw://sources` as
    readable URIs, but `read_resource_by_uri` does not route them — calls to
    `read_knowledge_uri` or `resources/read` with those URIs would error out.
    Removed them from the table and added a note pointing to `resources/list`
    and the `list_pages_by_*` tools as the real enumeration paths.
  - Added `wiki://page/index` to the table to disambiguate it from
    `wiki://index`: the first is the navigable index generated by
    `rebuild_wiki_index` (grouped by `type` and `project`); the second returns
    the `rawindex.md` (raw-source listing in `id | path | extracted_at` form).

## [0.1.8] - 2026-05-21

### Added

- **Full bidirectional Obsidian compatibility** for inline page links. Wiki
  bodies and Obsidian vaults now share the same link syntax — you can edit the
  same `.md` file in either environment without translation:
  - **Reading** (lenient parser): `extract_wiki_page_links` recognizes both
    `[[slug]]` / `[[slug|Display text]]` (Obsidian wikilinks) and the legacy
    `[Text](wiki://page/slug)` / bare `wiki://page/slug` forms in the same
    document. Backlinks, graph, orphans, link suggestions, and lint all benefit
    transparently.
  - **Writing**: `rebuild_wiki_index` now emits `[[slug]]` so generated index
    pages are first-class navigation in Obsidian.
- **Automatic schema migration on boot** (`migration` module). Existing wikis
  with legacy `wiki://page/X` body links are converted **once** to wikilink
  syntax the next time the server starts — no skill action, no tool call, no
  manual step:
  - Gated by a `.advwiki/.schema-version` marker (idempotent — never runs
    twice).
  - Atomic per-file (write-temp + rename) and re-entrant: a crash mid-migration
    is resumed safely on the next boot.
  - Creates a full backup of `.advwiki/pages/` under
    `.advwiki/.backup-pre-wikilinks-{timestamp}/` before the first write.
  - Logs a summary entry to `.advwikilog.md` (`[migration] v0 → v2: N pages
    processed, N changed, N links converted`).
  - Supports `ADVWIKI_MIGRATION_DRYRUN=1` to preview the conversion without
    writing.
  - Honors markdown context: code blocks (` ``` `), inline code (`` ` ``)
    outside of Claims `Source:` fields, frontmatter, and `raw://` URIs are all
    preserved as-is.

### Changed

- `## Claims` blocks: `Source: \`wiki://page/X\`` is migrated to
  `Source: [[X]]` (wiki-page sources only). Raw sources continue to use
  ``Source: `raw://source/{id}` ``.
- The `advwiki-memory` skill template and Claims examples now use `[[slug]]`
  and `[[slug|Display]]` for inline body links. `wiki://` is documented as the
  protocol-level URI used by `read_knowledge_uri`, `resources/read`, and tool
  arguments.

### Notes

- `wiki://page/{slug}` remains valid wherever it appears as an **MCP protocol
  URI** (tool arguments, search index keys, return messages, resources API).
  This is invisible to Obsidian and stays unchanged.
- The lenient parser stays in place permanently as a robustness measure —
  pages pasted from external sources or restored from old backups continue to
  work.



### Added

- **Link graph & backlinks** (`graph` module): the wiki can now be navigated as
  a knowledge graph. Edges come from `wiki://page/` links in page bodies and
  from the frontmatter `related` field.
  - `wiki_graph`: renders the link graph in `summary`, `full`, or `mermaid`
    format.
  - `backlinks`: lists which pages point to a given page.
  - `orphans`: lists pages with no incoming links (links from the generated
    `index` page are ignored, so it does not mask real orphans).
  - `related_pages`: lists pages related to a page, classifying each
    relationship (bidirectional, declared in `related`, links-to, linked-from).
  - `link_suggestions`: suggests links between not-yet-connected pages, ranking
    by content similarity plus boosts for shared `project` and tags.
- **Change planning with diff before writing** (`change_plan` module),
  covering roadmap item 5:
  - `propose_page_update`: stores a reviewable change proposal under
    `.advwiki/proposals/<id>.json` and returns a unified diff between the
    current and proposed page content, without writing the page.
  - `apply_page_update`: applies a proposal by id, re-checking via an MD5
    hash that the page has not changed since the proposal (overridable with
    `force`), then writes the page and records the operation in the log.
- **Traceable claims** (`claims` module), covering roadmap item 8: the
  `## Claims` block of a page lists statements with origin, confidence, and
  verification date (bilingual field labels).
  - `find_claims`: lists claims across the wiki, or for a single page.
  - `find_claims_without_source`: lists claims with no documented origin.
  - `find_conflicting_claims`: heuristic triage — flags claim pairs with
    overlapping vocabulary as conflict-review candidates (it does not detect
    contradiction).
  - `verify_claim`: updates a claim's `Last verified` date.
- `-h` / `--help` now prints the binary version.

### Changed

- `extract_wiki_page_links` moved from the `lint` module to the new `graph`
  module; `lint_wiki` keeps reusing it.
- Marked roadmap items 1-5 and 8 as implemented in `roadmap.md` and
  `roadmap-pt.md`, with a per-item status note describing the delivered tools
  and remaining gaps.

## [0.1.6] - 2026-05-15

This release covers roadmap phase 1: the foundation for a structured,
navigable wiki.

### Added

- **YAML frontmatter support** (`frontmatter` module): parsing and manipulation
  of structured page metadata — `type`, `project`, `status`, `created_at`,
  `updated_at`, `confidence`, `sources`, `related`, `tags`, and `owner` —
  including automatic date updates on write.
- **`lint_wiki` module**: structural and quality checks with `quick` and `all`
  scopes, detecting broken links, orphan pages, missing frontmatter, raw
  sources without a derived page, stale pages, decisions without rationale,
  and duplicate/similar pages.
- **Page classification tools**: `list_pages_by_type`, `list_pages_by_project`,
  and `list_pages_by_tag` filter pages by their frontmatter fields.
- **`find_pages_without_sources`**: lists pages with no `sources` entry in the
  frontmatter, surfacing candidates for review or linking to raw sources.
- **`rebuild_wiki_index`**: generates the navigable wiki index page
  (`wiki://page/index`), grouping pages by type and project from the
  frontmatter — distinct from the raw-source `rawindex.md`.

### Changed

- Refactored the `lint_wiki` tool: streamlined logic and an updated report
  format.
- Expanded README documentation and guidance for the root and global indices.

## [0.1.5] - 2026-05-14

### Added

- Exposed the `delete_page` and `delete_raw_source` tools.

### Fixed

- Fixed a bug in `delete_raw_source`.

### Changed

- Aligned the README with the real API.

## [0.1.4] - 2026-05-11

### Added

- Portuguese roadmap file (`roadmap-pt.md`).

### Changed

- Clarified documentation strings in `main.rs` and adjusted configuration
  example paths.

## [0.1.3] - 2026-05-11

### Changed

- Moved the npm publish logic into a dedicated workflow file and improved the
  npm publish workflow (Node.js v24, better artifact handling and tarball
  inspection).

## [0.1.2] - 2026-05-11

### Changed

- Release workflow adjustments for npm publishing.

## [0.1.1] - 2026-05-11

### Changed

- README adjustments for the npm install command.

## [0.1.0] - 2026-05-09

### Added

- Initial release: a local MCP wiki server running over `stdio` with
  JSON-RPC 2.0, backed by Tantivy BM25 full-text search.
- Core tools: `query_wiki`, `update_page`, `ingest_source`,
  `ingest_extracted_content`, `lint_wiki`, and `read_knowledge_uri`.
- GitHub release workflow with cargo-dist and an npm installer.

[0.1.9]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/dfalci/mcp-advwiki/releases/tag/v0.1.0
