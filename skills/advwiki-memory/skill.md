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

## Available AdvWiki tools

Use the actual MCP tools exposed by AdvWiki:

- `query_wiki` — search relevant wiki pages before answering.
- `read_knowledge_uri` / `resources/read` — read a known `wiki://...` URI.
- `update_page` — create or update curated pages.
- `propose_page_update` — prepare a reviewable diff without writing.
- `apply_page_update` — apply a previously proposed update.
- `ingest_extracted_content` / `ingest_source` — store raw or semi-raw evidence.
- `list_pages_by_project`, `list_pages_by_type`, `list_pages_by_tag` — browse pages.
- `wiki_graph`, `backlinks`, `orphans`, `related_pages`, `link_suggestions` — inspect navigation.
- `find_claims`, `find_claims_without_source`, `find_conflicting_claims`, `verify_claim` — inspect or verify traceable claims.
- `lint_wiki`, `rebuild_wiki_index` — maintenance, only when useful or requested.

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
useful variations, including Portuguese/English variants when relevant.

Do at most 5 additional searches. Stop earlier if the recovered context is enough.

---

## How to answer after searching

If relevant context exists, use it before reasoning. Mention only what helps the
user, preferably with the page URI.

Suggested format:

```text
📚 Contexto recuperado da wiki:
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

- “registre isso na wiki”;
- “salve essa decisão”;
- “documente esse padrão”;
- “atualize a página X”;
- “coloque isso na memória do projeto”;
- “guarde esse contexto para futuras sessões”.

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
  - Source: `<precise source>`
  - Confidence: high|medium|low
  - Last verified: YYYY-MM-DD

## See also
- [Related page](wiki://page/<slug>): <why it matters>
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
  - Source: `file/path/or/wiki/source`
  - Confidence: high
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

Call `rebuild_wiki_index` after bulk imports, major reorganizations, or when the
user asks to rebuild/update navigation.

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
- creating isolated pages without links.

---

## Final rule

For project-specific technical questions, combine:

1. recovered AdvWiki context;
2. current evidence from the user;
3. source code, logs, or official documentation when available;
4. clear technical reasoning.

Use AdvWiki to preserve continuity, not to replace judgment.
