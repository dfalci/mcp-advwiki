---
name: advwiki-memory
description: |
  Instructs Claude to use the advwiki MCP server as persistent architectural memory for software projects — especially microservice architectures. Use this skill whenever the user mentions a project, service, repository, architectural decision, investigated bug, integration pattern, or any technical knowledge worth preserving across sessions. It should also be activated when the user asks to "register", "save", "note", "document in the wiki", "search the wiki", "what do I already know about X", "recover context for Y", or any variation of those intents. If the user is starting to discuss a microservice or system component, this skill should be consulted immediately to guide search and recording behavior.
---

# AdvWiki Memory — Microservice Architectural Memory

This skill instructs Claude to use the **advwiki** MCP server as a persistent architectural memory layer across sessions, acting as a searchable knowledge base for the user's software projects.

---

## Central Principle

advwiki is the session's **source of persistent architectural context**. It should guide the answer, but it does not replace source code, current logs, official documentation, recent evidence, or explicit user instructions.

Before reasoning about any microservice, component, repository, or architectural decision in a known project, Claude must search what is already recorded. After learning something new and relevant, and only after receiving an explicit instruction from the user, Claude must record it in a structured way.

**Claude never writes to the wiki on its own initiative.** All writing must be explicitly requested by the user. Relevant reading is proactive and happens before answering architectural questions, as long as there is enough context for a useful search.

---

## Tools available in the advwiki MCP

| Tool | When to use |
|---|---|
| `query_wiki` | Search for context before answering about a project, service, component, or decision |
| `update_page` | Create or update a page when the user asks for curated recording |
| `ingest_extracted_content` | Save raw or semi-raw content extracted from logs, specs, code snippets, or messages |
| `lint_wiki` | Check index consistency or wiki integrity, rarely and only on request |
| `read_knowledge_uri` | Read any wiki URI directly (`wiki://page/{slug}`, `wiki://log`, etc.) |
| `list_pages_by_type` | List all pages of a given type (e.g. all `decision` or `service` pages) |
| `list_pages_by_project` | List all pages belonging to a given project |
| `list_pages_by_tag` | List all pages with a given tag |
| `find_pages_without_sources` | Identify pages with no linked raw sources — candidates for linkage or review |
| `rebuild_wiki_index` | Regenerate `wiki://page/index` after bulk changes or reorganizations |
| `resources/list` | List existing pages when the user wants to browse or locate pages |
| `resources/read` | Read a specific page by URI |

---

## Session Context Bootstrap

When this skill is activated and a project, repository, service, or module is clearly identified in the user's message, Claude should perform a lightweight initial read to understand the wiki map before the first answer.

Preferred order:

1. If the wiki has a structured index page:
   - read `wiki://page/index` for an overview of all pages grouped by type and project;
   - use this to understand the wiki's scope and locate relevant pages before answering.

2. If a known project root page exists or can be inferred:
   - read `wiki://page/{project}`;
   - use this page only to understand the documentation structure, existing modules, and relevant links.

3. If the root page is not known:
   - call `query_wiki` with the project, service, or component name;
   - use `maxPages: 3`;
   - use `includeRawReferences: false`, unless the question requires detailed traceability.

4. If no project, service, or component is clear:
   - do not query a global index;
   - wait for the first concrete project, service, repository, module, or component name.

The bootstrap should not be presented to the user in full. It is meant to guide navigation and improve the answer. Claude should only mention recovered context when it is relevant to the answer.

---

## Proactive Search Behavior

### When to search automatically

Claude must call `query_wiki` **before answering** whenever the user's question involves:

- the name of a microservice, module, repository, or component;
- an architectural decision (`"why do we use Kafka here?"`);
- an integration pattern between services (`"how does X talk to Y?"`);
- a bug or behavior investigated previously;
- environment, deployment, or infrastructure configuration for a specific project;
- existing documentation, technical history, or accumulated knowledge;
- questions with "how is it", "what is the pattern", "what was decided", "how does it work", "what do we know", "have we seen this before", or close variations.

Do not search automatically when:

- the question is generic and does not mention a specific project, service, component, or decision;
- the user explicitly asks for a conceptual explanation without project context;
- the search could bring too much context and contaminate an answer that should remain independent;
- the user asks not to query the wiki.

### How to execute the search

Default usage:

```text
Call: query_wiki
  question: "<terms derived from the user's question>"
  maxPages: 5
  includeRawReferences: false
```

Use `includeRawReferences: true` only when Claude needs to audit the source, compare evidence, prepare a wiki update, or when the user asks for detailed traceability.

Internal technical references should be used by the agent, but not exposed to the user unless they are useful for navigation, auditing, or traceability.

### Query construction

Extract the key concepts from the question without repeating the literal question. Since advwiki uses term-based search, prioritize real names of services, modules, technologies, queues, tables, endpoints, classes, and errors.

| User question | Wiki query |
|---|---|
| "how does the orders service handle payment failure?" | `"orders payment failure retry"` |
| "what is the authentication pattern in the microservices?" | `"authentication JWT token microservices"` |
| "what do we know about the stock service database?" | `"stock database schema"` |
| "have we investigated this ALB timeout error before?" | `"ALB timeout error investigation"` |

### How to use search results

- If relevant results were found: use the recovered context before reasoning.
- If the context is useful to the user, present a short summary indicating the source (`wiki://page/page-name`).
- If nothing was found: tell the user there is no recorded context for the topic and continue reasoning without prior memory.
- If the result is partial: use what exists and make the gaps explicit.
- If there is a conflict between the wiki and current evidence provided by the user, prioritize the current evidence and mention that the wiki may be outdated.

Suggested format when recovered context is relevant to the user:

```text
📚 Context recovered from the wiki:
— [page-name]: <summary of what was found>
— [another-page]: <summary>

Based on this: <answer>
```

### Additional searches

If the result is vague, incomplete, or apparently insufficient, Claude should perform additional searches with alternative terms.

Rules:

- Perform at most **5 additional searches**.
- Vary exact terms, service names, technologies, errors, aliases, and Portuguese/English words when appropriate.
- Do not repeat the same query with insignificant changes.
- Stop before 5 searches if the evidence found is already sufficient.
- After 5 additional searches without an adequate result, answer while stating that the recorded memory is insufficient.

Example:

```text
1. "payment retry"
2. "payment failure"
3. "pagamento retry"
4. "dead letter payment"
5. "orders payment timeout"
6. "order payment compensation"
```

---

## On-Demand Recording Behavior

### Claude only writes when the user explicitly asks

Triggers that indicate a recording request:

- "register this in the wiki";
- "save this decision";
- "note what we found";
- "document this pattern";
- "update the page for service X";
- "add this to what we already have about Y";
- "put this in the project memory";
- "keep this context for future sessions".

If the intent to record is ambiguous, Claude must ask before writing.

### What is worth recording

High priority — always record when requested:

- **Architectural decisions**: what was decided, why, and which alternatives were rejected;
- **Integration patterns**: how two services communicate, API contracts, queues, events;
- **Investigation findings**: root cause of bugs, confirmed unexpected behaviors;
- **Non-obvious configurations**: critical environment variables, flags, calibrated timeouts;
- **External dependencies**: third-party services, SDKs, and important peculiarities;
- **Project conventions**: code patterns, names, module structure, and agreed practices;
- **Known limitations**: accepted risks, technical debt, trade-offs, and weak points.

Low priority — record only if the user asks:

- implementation details that change frequently;
- complete code;
- information already present in the project's official documentation;
- long logs or outputs without synthesis.

### Raw content vs. curated knowledge

Use `ingest_extracted_content` to preserve raw or semi-raw material:

- logs;
- copied specs;
- code snippets;
- error outputs;
- long user messages;
- outputs from external tools.

Use `update_page` for curated knowledge:

- decisions;
- architecture;
- integrations;
- root causes;
- reusable patterns;
- consolidated investigation summaries;
- final organized documentation.

Whenever possible, raw content should be summarized or referenced by a curated page, preventing the wiki from becoming only a log dump.

---

## Slug Convention

Use consistent, specific, hierarchical slugs to make search easier:

```text
{service}-overview              → microservice overview
{service}-architecture          → architectural decisions for the service
{service}-integration-{other}   → how two services communicate
{service}-database              → schema, indexes, data decisions
{service}-known-bugs            → documented problematic behaviors
{service}-deployment            → infrastructure, environment, deployment configuration
{service}-flow-{name}           → relevant functional or technical flow
decision-{topic}                → cross-cutting decision affecting multiple services
pattern-{name}                  → reusable architectural pattern
project-{name}-index            → alternative index if the project root slug is not suitable
```

Concrete examples:

- `payment-overview`
- `orders-integration-stock`
- `decision-jwt-authentication`
- `pattern-retry-with-backoff`
- `apolo-sev-grpc-integration`

Avoid generic slugs such as `architecture`, `notes`, `bugs`, `deployment`, or `decision`.

---

## Writing Mode

- **`overwrite`**: when creating a page for the first time or completely rewriting a page after an explicit request.
- **`append`**: when adding information to an existing page without destroying what was already there.

Before using `overwrite` on an existing page, Claude must read the current page with `resources/read`, unless the user explicitly asks to replace everything.

Always include the `rationale` field with the reason for the change.

Examples of `rationale`:

- `"discovery during investigation of bug #2341"`
- `"architectural decision confirmed by the user in this session"`
- `"consolidation of integration pattern between orders and payment"`

---

## Page Template

When creating a new page, follow this structure and adapt sections to the concrete case:

```markdown
---
type: {service|decision|pattern|runbook|bug|note}
project: {project-name}
status: {active|draft|accepted|deprecated}
tags:
  - {tag1}
sources:
  - raw://source/{source-id}
related:
  - {other-slug}
---

# {Page Title}

> Context: {session, ticket, PR, or investigation}

## Summary
One sentence describing what this page documents.

## Context
The problem, component, or decision that motivated the page.

## {Main section}
Objective content, without redundancy.

## Decisions Made
- **What**: ...
- **Why**: ...
- **Rejected alternatives**: ...

## Points of Attention
Non-obvious behaviors, gotchas, known limitations, and risks.

## See also
- [Related page](wiki://page/other-slug): why this link is useful.

## References
- Internal links: `wiki://page/other-slug`
- External links: relevant URLs
```

Not every page needs all sections. Claude should remove empty sections instead of filling them with artificial content.

---

## Root Index and Cross-Navigation

In addition to creating or updating the target page, Claude should keep the wiki easy to navigate for someone who opened it without prior context.

### Root index rule

- The wiki has a structured global index at `wiki://page/index`, generated by `rebuild_wiki_index`. It groups all pages by `type` and `project` automatically. Call `rebuild_wiki_index` after bulk imports, reorganizations, or when the user asks to "rebuild the index" or "update the navigation".
- In addition to the global index, each project may have its own root page. Whenever relevant new pages are created for a project or module, Claude should suggest or execute, if it is part of the user's request, an update to the corresponding root page.
- Examples: `wiki://page/omnisiga`, `wiki://page/apolo`, `wiki://page/matchb2g`.
- The root page should work as a **navigation hub**, grouped intuitively by module, domain, or architectural topic.
- Avoid loose and generic lists; prefer sections such as `Botengine`, `Integrations`, `Cross-cutting decisions`, `Deployment`, `Database`, `Flows`, `Known bugs`.

### Cross-linking rule

Every new page should, whenever appropriate, contain internal links to sibling pages, parent pages, or related pages.

Examples:

- a `{service}-architecture` page should point to `{service}-overview`;
- a `{service}-flow-*` page should point to `{service}-architecture`;
- an `{a}-integration-{b}` page should point to the main pages of `a` and `b`;
- a cross-cutting `decision-*` page should point to the impacted services;
- a known bug page should point to the affected component page and, if applicable, to the decision that was made.

When updating an existing page, Claude should check whether useful links are missing and improve navigation and context understanding.

### Intuitive navigation heuristic

When writing or updating pages, Claude should think: "could a person who opened this page without knowing the wiki discover where to go next?"

To do that:

- include a `## See also` section when there are 2 or more relevant related links;
- use readable link titles, not only loose URIs, whenever the format allows it;
- prefer a few useful and well-contextualized links over many unexplained links;
- keep page relationships explicit: overview → architecture → flows → integrations → related decisions.

### When to relink old pages

If the user asks for an update, consolidation, organization, index, root page, navigation, or more intuitive documentation, Claude should treat the following as part of the task:

- update the project root page;
- add cross-links to related pages;
- reorganize navigation to reflect the existing pages.

---

## Sessions with Multiple Microservices

When the session involves two or more services at the same time:

1. Identify the involved services at the beginning of the conversation or when the first service name appears.
2. Search context for each one with separate `query_wiki` queries.
3. Keep slugs separate: do not mix information from different services in the same page, except in integration pages (`{a}-integration-{b}`).
4. When recording: if the information is service-specific, use that service's page; if it is cross-cutting, use a decision or pattern page.

Example of opening a multi-service session:

```text
[user mentions services A and B]
→ Claude calls query_wiki("service-A") and query_wiki("service-B")
→ Presents the recovered context when it is relevant
→ Continues the analysis using the context as a base
```

---

## Typical Session Flow

```text
1. User mentions a service, project, or architectural topic
        ↓
2. Claude performs lightweight bootstrap, if an identifiable project exists
        ↓
3. Claude calls query_wiki with relevant terms
        ↓
4. Claude presents recovered context when useful to the user
        ↓
5. Conversation and analysis happen with the context in mind
        ↓
6. User explicitly asks to record something
        ↓
7. Claude chooses the appropriate tool, slug, mode, and format
        ↓
8. Claude writes, updates links when necessary, and confirms the resulting URI
```

---

## Common Mistakes to Avoid

| Mistake | Correct behavior |
|---|---|
| Writing to the wiki without the user asking | Write only on explicit request |
| Treating the wiki as absolute truth | Use it as persistent context and compare it with current evidence |
| Searching only once and ignoring partial results | Perform up to 5 additional searches with alternative terms if the result is vague |
| Running a global search without a clear project | Wait or use only concrete terms mentioned by the user |
| Exposing raw references unnecessarily | Use `includeRawReferences: false` by default |
| Generic slug like `architecture` or `notes` | Specific slug like `payment-architecture` |
| Mixing information from multiple services in one page | One page per service/topic, separate integration pages |
| Recording ephemeral implementation details | Focus on decisions, patterns, and confirmed behaviors |
| Using `overwrite` on an existing page without checking | Read the current page before deciding between `overwrite` and `append` |
| Creating isolated pages without links | Update internal links and root index when appropriate |

---

## Notes on the Search Engine

advwiki uses BM25 with Tantivy, a term-based search engine — not semantic search. This means:

- **Exact terms carry more weight**: use real names of services, technologies, tables, queues, endpoints, classes, and errors.
- **Short and precise queries work better** than long sentences.
- **Try variations**: if `"payment retry"` returns nothing, try `"payment failure"`, `"pagamento retry"`, `"retry backoff"`, or the exact error name.
- **Quotes for exact phrases**: if the query supports it, `"dead letter queue"` is more precise than `dead letter queue`.
- **Do not rely only on synonyms**: BM25 does not understand semantic equivalence the way a vector model would.

---

## Final Rule

Claude should use the wiki to remember, guide, and organize architectural knowledge. It should avoid both forgetting and contamination from excessive context. The ideal answer combines:

1. persistent memory recovered from the wiki;
2. current evidence provided by the user;
3. clear technical reasoning;
4. structured recording only when requested by the user.
