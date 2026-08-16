---
name: advwiki-memory
description: >-
  Persistent project memory in a local Markdown wiki (AdvWiki, over MCP):
  architecture, decisions, integrations, investigated bugs, conventions and raw
  evidence. Use it when the user mentions a concrete project, repository,
  service, component, architectural decision, deployment detail or past
  investigation, or asks to search, record, update or verify project knowledge.
  Not for generic programming questions unless tied to a specific project.
---

# AdvWiki Memory

AdvWiki is the agent's persistent technical memory for real software projects.
It does not replace source code, logs, current evidence, official documentation,
or explicit user instructions.

It solves three problems: findings from one session should survive into the
next; large projects should be queried instead of loaded whole; and scattered
knowledge should be searchable from one place.

Core rule:

- **Read proactively** when the user asks about a known project, service,
  component, architecture, decision, integration, deployment, bug, or prior
  investigation.
- **Write only when explicitly requested** by the user.

The MCP tool descriptions carry the mechanics — arguments, defaults, slug rules,
section editing, error behavior. This skill covers judgment: when to reach for
the wiki, what belongs in it, and how pages should be shaped.

---

## Link syntax (Obsidian-compatible)

Page bodies use **wikilinks**: `[[slug]]` or `[[slug|Display text]]`. The legacy
`[Text](wiki://page/slug)` form is still read, but new content must not use it.

`wiki://` URIs remain valid as **MCP identifiers** — tool arguments and return
values — never as inline body links.

| Where | Format |
|---|---|
| Inline link in page body | `[[slug]]` or `[[slug\|Display]]` |
| Frontmatter `related` | bare slug: `- queue-service` |
| MCP tool argument | `wiki://page/{slug}` or bare slug |
| Claims `Source` field | `[[slug]]` for pages, `raw://source/{id}` for raw sources |

---

## When to search

Call `query_wiki` before answering when the user mentions a concrete project,
repository, service, module, component, class, endpoint, queue, table,
deployment or environment; an architectural decision or trade-off; an
integration; a previously investigated bug, error or incident; or asks "how does
X work?", "what did we decide?", "have we seen this before?".

Do **not** search when the question is generic and independent of project
context, when the user asks you not to, or when no concrete topic is
identifiable.

If results are weak, retry with variations — alternative spellings, or the
language the content was originally written in. Do at most 5 additional
searches, and stop earlier once the recovered context is enough.

---

## How to answer after searching

If relevant context exists, use it before reasoning. Mention only what helps the
user, preferably with the page URI:

```text
📚 Recovered wiki context:
- `wiki://page/<slug>`: <short useful summary>

<answer based on recovered context + current evidence>
```

If nothing relevant is found, say there is no recorded context for the topic and
continue with normal reasoning.

If the wiki conflicts with evidence the user provided, prioritize the current
evidence and say the wiki may be outdated.

---

## When to write

Never write to AdvWiki on your own initiative. Write only when the user asks —
"record this in the wiki", "save this decision", "document this pattern",
"update page X", "keep this context for future sessions". If the intent to
record is ambiguous, ask first.

Worth recording: architectural decisions and rejected alternatives; integration
patterns and contracts; confirmed root causes; non-obvious configuration,
timeouts, flags and limits; external dependency behavior; project conventions
and module structure; known limitations, risks and technical debt.

Not worth recording as curated documentation: unstable implementation details,
code dumps, long logs. Those are raw evidence — store them with
`ingest_extracted_content` and summarize the durable part into a curated page
that references them.

---

## Slug convention

Use specific slugs. Avoid generic names like `architecture`, `bugs`, `notes`,
or `decision`.

```text
{project}                         → project root/navigation page
{service}-overview                → service summary
{service}-architecture            → service architecture
{service}-integration-{other}     → integration between services/systems
{service}-database                → database/schema/index decisions
{service}-deployment              → infrastructure and environment
{service}-known-bugs              → known bugs and recurring issues
{service}-flow-{name}             → important flow
decision-{topic}                  → cross-cutting decision
pattern-{name}                    → reusable pattern
```

Examples: `orders-architecture`, `orders-integration-payment`,
`decision-jwt-authentication`, `pattern-retry-with-backoff`.

---

## Writing safety

When creating a new page, `overwrite` is fine. When changing an existing page:

1. read the current page first, unless the user explicitly asks to replace it;
2. prefer `propose_page_update` when the change is substantial or risky, and
   show the diff before applying;
3. use `apply_page_update` only after the user approves;
4. always give a rationale.

Prefer a section-scoped edit over a full `overwrite` — a full overwrite forces
you to reproduce the whole page and risks silently dropping content.

---

## Minimal page template

Adapt it. Remove empty sections.

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
  - Source: [[<source-slug>]]
  - Confidence: high|medium|low
  - Last verified: YYYY-MM-DD

## See also
- [[<slug>]] — <why it matters>
```

---

## Claims rules

Use a `## Claims` block only for load-bearing facts that may need future
verification: intervals, queue semantics, retry behavior, hard-coded limits,
security-relevant behavior, external service assumptions, non-obvious
invariants.

Syntax must be exact — each claim is a top-level `-` bullet with 2-space-indented
metadata, and the labels are `Source`, `Confidence` and `Last verified` (the
server accepts localized aliases for legacy content, but new claims use these):

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

Prefer precise sources, do not turn every sentence into a claim, and remember
that editing claims is writing to the wiki — it requires an explicit request.

---

## Navigation

Keep the wiki navigable. When creating or reorganizing pages, link to parent,
sibling, integration, decision and runbook pages. Project root pages work as
navigation hubs grouped by module or domain — overview, services, integrations,
cross-cutting decisions, deployment, database, flows, known bugs.

The navigable index page (`wiki://page/index`) is regenerated by the server
whenever pages change: there is no rebuild step and no tool to call.

Reach for `wiki_graph`, `backlinks`, `orphans`, `related_pages` and
`link_suggestions` when the task is to organize, audit, consolidate or improve
navigation.

---

## Multiple services

Search each service separately. Keep service-specific knowledge in
service-specific pages, put cross-service behavior in integration pages, and
cross-cutting rules in decision or pattern pages. Do not merge unrelated service
details into one page just because they came up in the same session.

---

For project-specific technical questions, combine recovered wiki context,
current evidence, source code and documentation. AdvWiki preserves continuity;
it does not replace judgment.
