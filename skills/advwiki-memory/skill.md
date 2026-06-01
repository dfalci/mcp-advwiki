---
name: advwiki-memory
description: |
  Use this skill when working on a concrete software project whose technical
  context may already exist in AdvWiki or should be preserved there by explicit
  user request.

  AdvWiki is a local Markdown wiki exposed through MCP. It gives the agent a
  searchable, persistent project memory across sessions: architecture notes,
  service descriptions, integration patterns, investigated bugs, operational
  findings, raw sources, claims, and curated documentation.

  Activate this skill when the user mentions a real project, repository,
  service, module, component, architectural decision, integration, deployment
  detail, recurring bug, or asks to search, recover, register, save, document,
  update, organize, or verify knowledge in the wiki.

  Do not activate it for generic programming questions unless the user connects
  the question to a specific project or explicitly asks to use AdvWiki.
---

# AdvWiki Memory

AdvWiki is the agent's persistent technical memory for real software projects.
It is not a replacement for source code, logs, current evidence, official
project documentation, or explicit user instructions.

Use AdvWiki to solve three practical problems:

1. **Forgotten context** — useful findings from one session should be recoverable
   in future sessions.
2. **Large projects** — only relevant project knowledge should be retrieved,
   instead of loading entire repositories or long histories.
3. **Scattered knowledge** — architecture, decisions, bugs, logs, integrations,
   and conventions should be searchable from one local wiki.

Core rule:

- **Read proactively** when the user asks about a known project, service,
  component, architecture, decision, integration, deployment, bug, or prior
  investigation.
- **Write only when explicitly requested** by the user.

---

## Link syntax (Obsidian-compatible)

Page bodies use **wikilink syntax** `[[slug]]` or `[[slug|Display text]]` —
the same as Obsidian. The server also still accepts the legacy form
`[Text](wiki://page/slug)` and bare `wiki://page/slug` for reading, but **new
content must use `[[slug]]`**. Existing wikis are migrated automatically on
server boot.

`wiki://` URIs remain valid as **MCP protocol identifiers** used by
`read_knowledge_uri`, `resources/read`, and tool return values
(`wiki://page/{slug}`, `wiki://log`, `wiki://index`, `wiki://rawindex`). Do not
use them as inline body links.

| Where | Format |
|---|---|
| Inline link in page body | `[[slug]]` or `[[slug\|Display]]` |
| Link in frontmatter `related` | bare slug: `- queue-service` |
| MCP tool arg (`read_knowledge_uri`, `backlinks`, ...) | `wiki://page/{slug}` or bare slug |
| Claims `Source` field | `[[slug]]` for wiki pages, `raw://source/{id}` for raw sources |

---

## Available AdvWiki tools

- `query_wiki` — BM25 search (`maxPages` default 10, `includeRawReferences` default false).
- `read_knowledge_uri` / `resources/read` — read one `wiki://...` / `raw://...` URI.
- `update_page` — create or update; `mode` is `overwrite` or `append`. Pass the optional `section` (an existing heading title) to edit only that section: `overwrite` replaces its body, `append` adds to its end — the rest of the page is preserved untouched.
- `set_page_metadata` — edit an existing page's frontmatter without resending the body: `set` for scalar fields (`type`, `project`, `status`, `confidence`, `owner`), `add`/`remove` for list fields (`tags`, `related`, `sources`). Unmentioned fields — including custom ones — are kept; `created_at`/`updated_at` are server-managed and rejected.
- `propose_page_update` — returns `proposal_id` + unified diff; show the diff to the user before applying. Accepts the same optional `section` (replace-only): `content` is then just the new section body and the diff stays small and focused.
- `apply_page_update` — apply by `proposalId`; refuses if the page changed since (re-checked via MD5); pass `force: true` to override.
- `ingest_source` / `ingest_extracted_content` — store raw evidence. `source_id` is MD5 of the URI (stable); re-ingest the same URI needs `force: true`.
- `delete_page`, `delete_raw_source` — remove content; include `rationale` (logged).
- `list_pages_by_project`, `list_pages_by_type`, `list_pages_by_tag` — browse by frontmatter.
- `find_pages_without_sources` — audit pages with no `sources` field.
- `wiki_graph` (`format`: `summary` | `full` | `mermaid`), `backlinks`, `orphans`, `related_pages`, `link_suggestions` — navigation.
- `find_claims`, `find_claims_without_source`, `find_conflicting_claims` — inspect claims.
- `verify_claim` — re-validate a claim; `claimIndex` is **1-based**.
- `lint_wiki` — required `scope`: `quick` for everyday hygiene, `all` for full audit (adds stale pages, decisions without rationale, near-duplicates).

### Quick gotchas

- **Slug**: ASCII `[a-zA-Z0-9._-]` only. No spaces, no accents, no `/\:`. Cannot start/end with `.` or end with space. Windows reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`) are rejected.
- **Auto-managed**: `created_at` and `updated_at` are written by the server on every save — never set them manually.
- **Not readable**: `wiki://list` and `raw://sources` are documented elsewhere but the server does not route them. Use `resources/list` or `list_pages_by_*` to enumerate.
- **Transparent**: schema migration to wikilinks runs once on boot (backup + log); primary/secondary instance roles are automatic — no agent action needed.
- **Claim field labels**: use `Source`, `Confidence`, and `Last verified`. The server also accepts localized aliases for legacy content, but new claims must use the English labels.
- **Section editing**: `section` targets an existing **ATX heading** (`#`..`######`) by title — case-insensitive, with or without the `#`. The page must exist; an unknown or duplicated section name errors out without writing (no silent fallback). A `## X` edit includes its `### Y` subsections.

---

## When to search

Call `query_wiki` before answering when the user mentions a concrete:

- project, repository, service, module, component, class, endpoint, queue, table,
  deployment, or environment;
- architectural decision or trade-off;
- integration between services or external systems;
- previously investigated bug, error, incident, or unexpected behavior;
- question such as “how does X work?”, “what did we decide?”, “what do we know
  about Y?”, “have we seen this before?”, or equivalent.

Do **not** search when:

- the question is generic and independent of project context;
- the user explicitly asks not to use the wiki;
- no concrete project/component/topic is identifiable.

Default search:

```text
query_wiki(
  question: "<short query with real project/service/technology/error terms>",
  maxPages: 5,
  includeRawReferences: false
)
```

Use `includeRawReferences: true` only for audits, source checking, wiki updates,
conflict analysis, or when the user asks for traceability.

AdvWiki search is BM25/Tantivy, not semantic search. Prefer short queries with
exact terms: service names, modules, technologies, tables, queues, endpoints,
classes, configuration keys, and exact errors. If results are weak, retry with
useful variations, including alternative spellings or language variants that
match how the content was originally written.

Do at most 5 additional searches. Stop earlier if the recovered context is enough.

---

## How to answer after searching

If relevant context exists, use it before reasoning. Mention only what helps the
user, preferably with the page URI.

Suggested format:

```text
📚 Recovered wiki context:
- `wiki://page/<slug>`: <short useful summary>

<answer based on recovered context + current evidence>
```

If nothing relevant is found, say that there is no recorded context for the topic
and continue with normal reasoning.

If the wiki conflicts with evidence provided by the user, prioritize the current
evidence and say the wiki may be outdated.

---

## When to write

Never write to AdvWiki on your own initiative.

Write only when the user explicitly asks, with wording such as:

- "record this in the wiki";
- "save this decision";
- "document this pattern";
- "update page X";
- "put this in the project memory";
- "keep this context for future sessions".

If the intent to record is ambiguous, ask before writing.

Record high-value knowledge when requested:

- architectural decisions and rejected alternatives;
- integration patterns and contracts;
- confirmed root causes of bugs/incidents;
- non-obvious configuration, timeouts, flags, limits, and operational details;
- external dependency behavior;
- project conventions and module structure;
- known limitations, risks, trade-offs, and technical debt.

Avoid recording unstable implementation details, full code dumps, or long logs as
curated documentation. Store those as raw evidence and summarize them in curated
pages when useful.

---

## Raw evidence vs. curated pages

Use raw ingestion for evidence:

- logs;
- pasted specs;
- error outputs;
- code snippets;
- long user messages;
- tool outputs.

Use curated pages for durable knowledge:

- service overview;
- architecture;
- decisions;
- integrations;
- runbooks;
- known bugs;
- project conventions;
- synthesized investigation findings.

Whenever possible, curated pages should reference raw sources instead of becoming
log dumps.

---

## Slug convention

Use specific slugs. Avoid generic names like `architecture`, `bugs`, `notes`, or
`decision`.

Preferred patterns:

```text
{project}                         → project root/navigation page
{service}-overview                → service summary
{service}-architecture            → service architecture
{service}-integration-{other}     → integration between services/systems
{service}-database                → database/schema/index decisions
{service}-deployment              → infrastructure and environment
{service}-known-bugs              → known bugs and recurring issues
{service}-flow-{name}             → important flow
 decision-{topic}                 → cross-cutting decision
 pattern-{name}                   → reusable pattern
```

Examples:

- `orders-architecture`
- `orders-integration-payment`
- `decision-jwt-authentication`
- `pattern-retry-with-backoff`
- `apolo-sev-grpc-integration`

---

## Writing safety

When creating a new page, `overwrite` is acceptable.

When changing an existing page:

1. read the current page first, unless the user explicitly asks to replace it;
2. prefer `propose_page_update` when the change is substantial or risky;
3. use `apply_page_update` only after the user approves the proposal;
4. include a clear rationale/reason for the change.

Use `append` for small additive updates that should not disturb existing content.
Use `overwrite` on an existing page only when the user explicitly requests a full
replacement or after reviewing the current content.

To change a single section without resending the whole page, pass `section` to
`update_page` (or `propose_page_update`): `mode: overwrite` replaces that
section's body, `mode: append` adds to its end. The server reconstructs the rest
of the page byte-for-byte, so this is the safest choice for a localized edit —
prefer it over a full `overwrite`, which forces you to reproduce the entire page
and risks silently dropping content.

---

## Minimal page template

Adapt the template. Remove empty sections.

```markdown
---
type: service|decision|pattern|runbook|bug|note
project: <project-name>
status: active|draft|accepted|deprecated
tags:
  - <tag>
sources:
  - raw://source/<source-id>
related:
  - <related-slug>
---

# <Title>

## Summary
<One or two sentences explaining what this page records.>

## Context
<Why this knowledge matters.>

## Details
<Curated technical content.>

## Decisions Made
- **What**: ...
- **Why**: ...
- **Rejected alternatives**: ...

## Points of Attention
<Gotchas, limitations, risks, or operational notes.>

## Claims

- <Verifiable load-bearing fact.>
  - Source: [[<source-slug>]]    # for wiki pages
  - Confidence: high|medium|low
  - Last verified: YYYY-MM-DD

- <Another fact backed by external evidence.>
  - Source: `raw://source/<source-id>`    # for raw sources
  - Confidence: medium
  - Last verified: YYYY-MM-DD

## See also
- [[<slug>]] — <why it matters>
- [[<slug>|Custom display text]] — <why it matters>
```

---

## Claims rules

Use a `## Claims` block only for load-bearing facts that may need future
verification: intervals, queue semantics, retry behavior, hard-coded limits,
security-relevant behavior, external service assumptions, non-obvious invariants.

Syntax must be exact:

```markdown
## Claims

- Claim text in one line.
  - Source: [[some-wiki-page]]
  - Confidence: high
  - Last verified: 2026-05-19

- Another claim sourced from raw evidence.
  - Source: `raw://source/abc123`
  - Confidence: medium
  - Last verified: 2026-05-19
```

Rules:

- each claim is a top-level `-` bullet;
- metadata is 2-space-indented;
- prefer precise sources;
- do not turn every sentence into a claim;
- editing claims is writing to the wiki, so it requires explicit user request.

---

## Navigation and index

Keep the wiki navigable.

When creating or reorganizing pages, add useful internal links to parent, sibling,
integration, decision, or runbook pages.

Project root pages should work as navigation hubs, grouped by module/domain/topic,
for example:

- Overview
- Services
- Integrations
- Cross-cutting decisions
- Deployment
- Database
- Flows
- Known bugs

The navigable index page (`wiki://page/index`, grouped by `type` and `project`)
is regenerated automatically by the server whenever pages change — there is no
manual rebuild step and no tool to call.

Use `wiki_graph`, `backlinks`, `orphans`, `related_pages`, and `link_suggestions`
when the task is to organize, audit, consolidate, or improve wiki navigation.

---

## Multiple services

When a conversation involves multiple services:

1. search each service separately;
2. keep service-specific knowledge in service-specific pages;
3. create integration pages for cross-service behavior;
4. use decision or pattern pages for cross-cutting rules.

Do not mix unrelated service details into a single page merely because they were
discussed in the same session.

---

## Common mistakes

Avoid:

- writing without explicit user request;
- treating the wiki as absolute truth;
- using long semantic queries instead of exact terms;
- stopping after one weak search;
- exposing raw references unless useful;
- creating generic slugs;
- mixing multiple services in one page;
- storing unstable implementation details as durable decisions;
- overwriting existing pages without reading them;
- creating isolated pages without links;
- writing body links in the legacy `[Text](wiki://page/slug)` form — use
  `[[slug]]` or `[[slug|Display]]` so they render natively in Obsidian and
  in the wiki's own graph.

---

## Final rule

For project-specific technical questions, combine:

1. recovered AdvWiki context;
2. current evidence from the user;
3. source code, logs, or official documentation when available;
4. clear technical reasoning.

Use AdvWiki to preserve continuity, not to replace judgment.
