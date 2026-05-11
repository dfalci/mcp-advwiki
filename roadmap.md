# MCP AdvWiki — Roadmap

This roadmap organizes the next suggested evolutions for `mcp-advwiki`, with a focus on transforming the project from a local MCP search/wiki server into a layer of persistent, navigable, auditable architectural memory that is actually useful in day-to-day development.

The core idea is to evolve from a searchable wiki into a **living knowledge base**, maintained with the help of agents, but structured enough that it does not turn into just a pile of Markdown.

---

## 1. Create a navigable wiki `index.md`

### Goal

Create a central wiki navigation page, different from `rawindex.md`.

`rawindex.md` should continue to be the index of ingested raw sources. `index.md`, on the other hand, should work as the conceptual map of the wiki: which projects exist, which services are documented, which decisions were made, which patterns were recorded, and which pages are the most important.

### Why this matters

Today, text search solves specific questions, but it does not provide an overview of the knowledge base. An agent starting a session needs to quickly understand “what exists” before going around consulting random pages.

A well-maintained `index.md` allows for:

- smarter session bootstrap;
- better human navigation;
- better use by agents;
- organization by project, domain, service, decision, and pattern;
- reduction of orphan pages.

### Proposed structure

```text
.advwiki/
  pages/
    index.md
    project-1-visao-geral.md
    project-1-arquitetura.md
    project-2-visao-geral.md
    decisao-vetores-por-escopo.md
    padrao-tool-calling-estrito.md
```

Example content:

```markdown
# Wiki Index

## Projects

- [[project-1-visao-geral]] — overview of `project-1`
- [[project-2-visao-geral]] — overview of `project-2`

## Services

- [[microservice-1-integracao-externa]] — integration between `microservice-1` and an external API via gRPC
- [[microservice-2-ingestor]] — ingestion of external events by `microservice-2`

## Cross-Cutting Decisions

- [[decisao-vectores-por-escopo]]
- [[decisao-mcp-como-camada-de-orquestracao]]

## Patterns

- [[padrao-tool-calling-estrito]]
- [[padrao-rag-documental-com-citacao]]
```

### Possible tools

```text
rebuild_wiki_index
read_wiki_index
update_wiki_index
```

### Priority

High.

This item should come before more sophisticated features, because it immediately improves the usefulness of the wiki and creates a foundation for other automations.

---

## 2. Add YAML frontmatter to pages

### Goal

Add structured metadata at the top of Markdown pages.

The main content remains free-form Markdown, but frontmatter allows the system to understand type, project, status, tags, sources, related pages, update date, and confidence level.

### Why this matters

Without metadata, the wiki depends too much on text search. With frontmatter, it becomes possible to create smarter tools, such as:

- listing pages by project;
- listing decisions;
- finding obsolete pages;
- finding pages without a source;
- generating the index automatically;
- creating graphs and backlinks;
- filtering by status;
- prioritizing active pages;
- separating service docs, bugs, decisions, patterns, runbooks, etc.

### Example

```markdown
---
type: service
project: project-1
status: active
created_at: 2026-05-11
updated_at: 2026-05-11
confidence: medium
sources:
  - raw://source/session-2026-05-11
related:
  - project-1-arquitetura
  - microservice-1-integracao-externa
tags:
  - mcp
  - architecture
  - backend
---

# project-1 — Overview
```

### Suggested fields

```yaml
type: service | decision | pattern | bug | runbook | integration | overview | note
project: project-name
status: active | draft | deprecated | stale
created_at: ISO date
updated_at: ISO date
confidence: low | medium | high
sources: []
related: []
tags: []
owner: optional
code_refs: optional
```

### Possible tools

```text
list_pages_by_project
list_pages_by_type
list_pages_by_tag
find_stale_pages
find_pages_without_sources
```

### Priority

High.

Frontmatter is a foundation. The earlier it is adopted, the lower the future migration cost.

---

## 3. Strengthen `lint_wiki`

### Goal

Turn `lint_wiki` into a central tool for maintaining the quality of the knowledge base.

It should not just validate basic structure. It should help find real wiki problems: broken links, orphan pages, obsolete pages, decisions without justification, raw sources without a derived page, and recurring concepts that are still undocumented.

### Why this matters

A wiki maintained by agents can grow fast, but it can also degrade fast.

Common problems:

- duplicate pages;
- broken links;
- pages without a source;
- decisions without context;
- outdated pages;
- documentation that contradicts another page;
- important information hidden only in raw sources;
- pages that are too large;
- important concepts repeated in several places without their own page.

`lint_wiki` should work like an “architectural reviewer” for the wiki.

### Suggested scopes

```text
quick:
  - missing frontmatter
  - broken links
  - orphan pages
  - pages without recent updates
  - pages without a "See also"
  - raw sources without a derived page

full:
  - possible contradictions
  - recurring concepts without their own page
  - decisions without rationale
  - cited integrations without a dedicated page
  - overly large pages
  - pages with low confidence
  - stale pages due to code changes
```

### Example output

```markdown
# Wiki Lint Report

## Broken links

- `project-1-arquitetura` points to `project-1-deploy`, but the page does not exist.

## Orphan pages

- `bug-cdn-spa-access-denied`
- `decisao-mcp-tools`

## Recurring concepts without their own page

- "vector store per scope" appears in 4 pages, but there is no dedicated page.

## Decisions without rationale

- `decisao-cache-de-sessao`
```

### Priority

High.

After `index.md` and frontmatter, this is probably the item with the biggest practical impact.

---

## 4. Create a link graph and backlinks

### Goal

Allow the wiki to be navigated as a knowledge graph.

Pages should be able to point to each other using Obsidian-style links:

```markdown
See also:
- [[project-1-arquitetura]]
- [[microservice-1-integracao-externa]]
- [[decisao-vetores-por-escopo]]
```

The system should be able to discover:

- which pages point to a page;
- which pages receive no links;
- which pages are hubs;
- which decisions affect which services;
- which integrations connect which components.

### Why this matters

In software projects, knowledge is not linear. A decision may affect several services; an integration may depend on a cross-cutting decision; a bug may reveal an architectural problem.

The graph helps both humans and agents understand relationships.

### Possible tools

```text
wiki_graph
backlinks
orphans
related_pages
link_suggestions
```

### Example usage

```text
backlinks(uri="wiki://page/decisao-vetores-por-escopo")
```

Expected output:

```markdown
# Backlinks for `decisao-vetores-por-escopo`

- `project-1-arquitetura`
- `project-1-documentos`
- `project-1-sessao-chat`
```

### Priority

Medium-high.

This should come after `index.md` and frontmatter, because it depends on a minimum amount of wiki organization.

---

## 5. Introduce change planning and diff before writing

### Goal

Reduce the risk of the agent degrading existing pages by using only `append` or `overwrite`.

Instead of writing directly, the MCP can offer an intermediate step: propose the change before applying it.

### Why this matters

`append` is simple, but it can create repetitive and disorganized pages.

`overwrite` is powerful, but dangerous.

A flow with plan and diff makes it possible to:

- review what will be changed;
- know which sections will be touched;
- preserve existing content;
- audit the reasoning behind the change;
- avoid accidental loss;
- allow human approval, when necessary.

### Suggested tools

```text
propose_page_update
apply_page_update
```

### Example plan

```json
{
  "target": "wiki://page/project-1-arquitetura",
  "operation": "patch",
  "reason": "new decision about vector store scope",
  "changes": [
    {
      "section": "Decisions Made",
      "action": "add_bullet",
      "content": "Separate vector stores into tenant, project, and session."
    }
  ],
  "affected_links": [
    "project-1-visao-geral",
    "decisao-vetores-por-escopo"
  ]
}
```

### Priority

Medium-high.

It becomes especially important when the wiki starts being used in real projects and with long pages.

---

## 6. Create semantic domain tools

### Goal

Add more specific tools that capture architectural knowledge in a structured way, instead of depending only on `update_page`.

### Why this matters

Generic tools are flexible, but they demand too much from the agent. Semantic tools reduce ambiguity and improve the quality of generated content.

Instead of asking the agent to create free-form Markdown, the MCP can offer specific contracts for common kinds of knowledge.

### Suggested tools

```text
record_architecture_decision
record_bug_investigation
record_integration_pattern
record_service_overview
record_deployment_note
record_runbook
record_external_dependency
```

### Example: architectural decision

```json
{
  "decision_id": "decisao-vetores-por-escopo",
  "project": "project-1",
  "title": "Separate vector stores by scope",
  "context": "The product needs to handle organization-wide shared documents, workspace-level files, and session attachments.",
  "decision": "Use separate vector stores for tenant, workspace, and session.",
  "alternatives_rejected": [
    "a single global vector store",
    "session attachments only"
  ],
  "consequences": [
    "improves isolation",
    "increases routing complexity"
  ],
  "related_pages": [
    "project-1-arquitetura",
    "project-1-documentos"
  ]
}
```

### Priority

Medium.

Very useful, but it is better to create this after the page format is stabilized.

---

## 7. Create a curated ingestion flow

### Goal

Evolve raw content ingestion into a flow where the MCP helps transform sources into curated knowledge.

Today, ingesting a raw source preserves content, but does not necessarily update the main wiki.

A curated ingestion flow should:

1. save the raw content;
2. search for related pages;
3. propose pages to create or update;
4. point out possible contradictions;
5. update index and links;
6. log the operation;
7. run quick lint.

### Why this matters

The main value of the wiki is not in storing raw text. It is in transforming raw text into organized, summarized, linked, and useful pages.

### Suggested tools

```text
ingest_extracted_content
propose_ingest_plan
apply_ingest_plan
```

### Example plan

```markdown
# Ingestion Plan

## Saved source

- `raw://source/session-2026-05-11`

## Related pages found

- `project-1-arquitetura`
- `project-1-documentos`
- `decisao-vetores-por-escopo`

## Proposed updates

- Create `project-1-politica-documentos`
- Update `project-1-arquitetura`
- Add a backlink to `project-1-visao-geral`

## Possible gaps

- There is no document reindexing runbook.
```

### Priority

Medium.

It is a natural evolution after semantic tools and diff.

---

## 8. Record traceable claims

### Goal

Allow important wiki statements to have origin, confidence, and verification date.

### Why this matters

Software architecture changes. A wiki can become obsolete or contain claims without grounding.

Traceable claims make it possible to answer:

- where did this information come from?
- is this statement still reliable?
- when was it last verified?
- is there another page saying the opposite?
- did this come from code, log, conversation, documentation, or an explicit decision?

### Example

```markdown
## Claims

- The platform uses three minimum vector store scopes: tenant, workspace, and session.
  - Source: `wiki://page/decisao-vetores-por-escopo`
  - Confidence: high
  - Last verified: 2026-05-11

- `microservice-2` communicates with a processing API via bidirectional gRPC.
  - Source: `raw://source/session-grpc-microservice-2-api-2026-05-11`
  - Confidence: high
  - Last verified: 2026-05-11
```

### Suggested tools

```text
find_claims
find_claims_without_source
find_conflicting_claims
verify_claim
```

### Priority

Medium.

It is an advanced feature, but very valuable for avoiding “hallucinated memory”.

---

## 9. Integrate optional Git versioning

### Goal

Allow the wiki to be versioned with Git.

Since knowledge is stored as Markdown and local files, Git is a natural choice for history, diff, rollback, and collaboration.

### Why this matters

A wiki maintained by agents needs to be auditable.

Git provides:

- change history;
- comparison between versions;
- rollback;
- branches;
- commits with semantic messages;
- human review via pull request;
- synchronization with a remote repository.

### Suggested mode

Add an optional flag:

```text
mcp-advwiki --root <PATH> --git
```

### Suggested tools

```text
wiki_git_status
wiki_git_diff
wiki_git_commit
wiki_git_history
wiki_git_rollback
```

### Example commit

```text
docs(project-1): record vector store scoping decision
```

### Priority

Medium.

It does not need to be mandatory in the first version, but it is an excellent option for real-world use.

---

## 10. Detect obsolescence through source code references

### Goal

Allow wiki pages to point to code files and let the system detect when those files change.

### Why this matters

Architectural documentation becomes obsolete mainly when the code changes and the wiki does not follow along.

If a page documents a class, module, or flow, it can record code references and last verification hashes.

### Example frontmatter

```yaml
code_refs:
  - path: src/microservice_1/handler.rs
    last_seen_hash: abc123
  - path: src/project_1/schema.rs
    last_seen_hash: def456
```

### Example lint

```markdown
# Possibly obsolete pages

- `flow-microservice-1-handler`
  - References `handler.rs`
  - The file has changed since the last verification.
```

### Suggested tools

```text
scan_code_refs
mark_code_refs_verified
find_stale_code_refs
```

### Priority

Medium.

For an architectural software wiki, this feature could become a major differentiator.

---

## 11. Prepare optional hybrid search

### Goal

Prepare the search engine to support, in the future, modes beyond BM25.

BM25 is simple, fast, and excellent for exact terms. But with a large wiki, it may be useful to add semantic search and reranking.

### Why this matters

The wiki can grow and the questions can become more semantic:

- “how do we handle temporary integration failure?”
- “which decision explains this retry pattern?”
- “where do we document tenant isolation?”

Those questions do not always use the same terms as the pages.

### Suggested modes

```rust
enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}
```

### Suggested tool

```json
{
  "question": "how does the document upload flow work?",
  "mode": "hybrid",
  "maxPages": 8
}
```

### Priority

Low-medium.

This should not be a priority before improving the wiki structure. A well-organized wiki with BM25 can already go a long way.

---

## 12. Create smarter session bootstrap

### Goal

Offer a dedicated tool to retrieve the initial context of a project or service.

Instead of the agent making several loose searches, it could call:

```text
bootstrap_context(project="project-1")
```

### Why this matters

When starting a conversation about a project, the agent needs to know:

- which central pages exist;
- which recent decisions were recorded;
- which gaps are known;
- which pages are stale;
- which services or modules are relevant.

### Example output

```markdown
# Initial context — `project-1`

## Central pages

- `project-1-visao-geral`
- `project-1-arquitetura`
- `microservice-1-integracao-externa`

## Recent updates

- 2026-05-11: vector store decision recorded.
- 2026-05-10: CDN issue for SPA documented.

## Possible gaps

- There is no page about document versioning policy.
- There is no complete deployment runbook.
```

### Priority

Medium-high.

After `index.md`, this feature greatly improves day-to-day use of the skill.

---

## 13. Create an architectural review mode

### Goal

Add a diagnostic capability for the wiki/project itself.

The tool would not answer a specific question. It would review the architectural memory and suggest improvements.

### Why this matters

Over time, the wiki may reveal gaps in the project itself:

- implicit decisions that are still not recorded;
- recurring risks;
- missing runbooks;
- integrations without a documented contract;
- weakly documented areas;
- patterns being used but not named.

### Suggested tool

```text
review_project_memory
```

### Example output

```markdown
# Architectural Diagnosis — `project-1`

## Well-documented decisions

- Vector stores by scope
- gRPC communication with a processing API

## Implicit decisions, but not yet recorded

- Multi-tenant isolation strategy
- Session and attachment retention policy

## Architectural risks

- Missing recovery runbook for gRPC channel failure.
- Missing documentation for attachment limits per session.

## Recommended next pages

- `project-1-runbook-grpc-microservice-1`
- `project-1-politica-retencao-documentos`
- `decisao-isolamento-multitenant`
```

### Priority

Medium.

This could become one of the most interesting features in the project, especially for use by architects and tech leads.

---

# Suggested implementation order

## Phase 1 — Wiki foundation

1. Create navigable `index.md`.
2. Add YAML frontmatter.
3. Improve `lint_wiki` for links, orphans, and metadata.
4. Create basic backlinks/graph.

## Phase 2 — Safer writing

5. Create change plan + diff.
6. Create semantic domain tools.
7. Create curated ingestion flow.

## Phase 3 — Quality and auditability

8. Record traceable claims.
9. Integrate optional Git.
10. Detect obsolescence through code references.

## Phase 4 — Usage intelligence

11. Prepare optional hybrid search.
12. Create session bootstrap.
13. Create architectural review mode.

---

# Success criteria

`mcp-advwiki` should evolve into a system where the agent can:

- quickly discover what is already known about a project;
- answer using persistent memory without inventing context;
- record new decisions in a structured way;
- preserve raw sources without mixing them with curated knowledge;
- keep links and the index navigable;
- detect obsolete documentation;
- review the quality of the wiki itself;
- allow auditing of changes made by the agent.

In short: the goal is not just to have local search. The goal is to create a **persistent, navigable, auditable, and incremental architectural memory** for software projects.
