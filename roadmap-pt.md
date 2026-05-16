# MCP AdvWiki — Roadmap

Este roadmap organiza as próximas evoluções sugeridas para o `mcp-advwiki`, com foco em transformar o projeto de um servidor MCP de busca/wiki local em uma camada de memória arquitetural persistente, navegável, auditável e útil no dia a dia de desenvolvimento.

A ideia central é evoluir de uma wiki pesquisável para uma **base de conhecimento viva**, mantida com ajuda de agentes, mas estruturada o suficiente para não virar apenas um amontoado de Markdown.

---

## ~~1. Criar um `index.md` navegável da wiki~~ ✅ Implementado

> **Status:** Implementado — a tool `rebuild_wiki_index` gera a página `wiki://page/index`, agrupando as páginas por tipo e projeto a partir do frontmatter.

### Objetivo

Criar uma página central de navegação da wiki, diferente do `rawindex.md`.

O `rawindex.md` deve continuar sendo o índice das fontes brutas ingeridas. Já o `index.md` deve funcionar como o mapa conceitual da wiki: quais projetos existem, quais serviços estão documentados, quais decisões foram tomadas, quais padrões foram registrados e quais páginas são mais importantes.

### Por que isso é importante

Hoje, uma busca textual resolve perguntas pontuais, mas não oferece uma visão geral da base de conhecimento. Um agente que inicia uma sessão precisa entender rapidamente “o que existe” antes de sair consultando páginas aleatórias.

Um `index.md` bem mantido permite:

- bootstrap mais inteligente da sessão;
- melhor navegação humana;
- melhor uso por agentes;
- organização por projeto, domínio, serviço, decisão e padrão;
- redução de páginas órfãs.

### Proposta de estrutura

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

Exemplo de conteúdo:

```markdown
# Índice da Wiki

## Projetos

- [[project-1-visao-geral]] — visão geral do `project-1`
- [[project-2-visao-geral]] — visão geral do `project-2`

## Serviços

- [[microservice-1-integracao-externa]] — integração do `microservice-1` com uma API externa via gRPC
- [[microservice-2-ingestor]] — ingestão de eventos externos pelo `microservice-2`

## Decisões Transversais

- [[decisao-vectores-por-escopo]]
- [[decisao-mcp-como-camada-de-orquestracao]]

## Padrões

- [[padrao-tool-calling-estrito]]
- [[padrao-rag-documental-com-citacao]]
```

### Possíveis tools

```text
rebuild_wiki_index
read_wiki_index
update_wiki_index
```

### Prioridade

Alta.

Este item deve vir antes de recursos mais sofisticados, porque melhora imediatamente a utilidade da wiki e cria uma base para outras automações.

---

## ~~2. Adicionar YAML frontmatter às páginas~~ ✅ Implementado

> **Status:** Implementado — módulo de frontmatter YAML (`type`, `project`, `status`, `created_at`, `updated_at`, `confidence`, `sources`, `related`, `tags`, `owner`) com parsing e atualização automática de datas, além das tools `list_pages_by_type`, `list_pages_by_project`, `list_pages_by_tag` e `find_pages_without_sources`. (`find_stale_pages` ainda não tem tool dedicada; a detecção de páginas stale vive hoje dentro do `lint_wiki`.)

### Objetivo

Adicionar metadados estruturados no início das páginas Markdown.

O conteúdo principal continua sendo Markdown livre, mas o frontmatter permite que o sistema entenda tipo, projeto, status, tags, fontes, páginas relacionadas, data de atualização e nível de confiança.

### Por que isso é importante

Sem metadados, a wiki depende demais de busca textual. Com frontmatter, é possível criar ferramentas mais inteligentes, como:

- listar páginas por projeto;
- listar decisões;
- encontrar páginas obsoletas;
- encontrar páginas sem fonte;
- gerar índice automaticamente;
- criar grafos e backlinks;
- filtrar por status;
- priorizar páginas ativas;
- separar documentação de serviço, bug, decisão, padrão, runbook etc.

### Exemplo

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
  - arquitetura
  - backend
---

# project-1 — Visão Geral
```

### Campos sugeridos

```yaml
type: service | decision | pattern | bug | runbook | integration | overview | note
project: nome-do-projeto
status: active | draft | deprecated | stale
created_at: data ISO
updated_at: data ISO
confidence: low | medium | high
sources: []
related: []
tags: []
owner: opcional
code_refs: opcional
```

### Possíveis tools

```text
list_pages_by_project
list_pages_by_type
list_pages_by_tag
find_stale_pages
find_pages_without_sources
```

### Prioridade

Alta.

O frontmatter é uma fundação. Quanto antes for adotado, menor o custo de migração futura.

---

## ~~3. Fortalecer `lint_wiki`~~ ✅ Implementado

> **Status:** Implementado — `lint_wiki` com escopos `quick` e `all` detecta links quebrados, páginas órfãs, frontmatter ausente, raw sources sem página derivada, páginas stale, decisões sem rationale e páginas duplicadas/similares. Checagens mais avançadas do escopo `full` (contradições, conceitos recorrentes sem página própria, páginas muito grandes, stale por mudança de código) ainda ficam pendentes.

### Objetivo

Transformar o `lint_wiki` em uma ferramenta central de manutenção da qualidade da base de conhecimento.

Ela não deve apenas validar estrutura básica. Deve ajudar a encontrar problemas reais da wiki: links quebrados, páginas órfãs, páginas obsoletas, decisões sem justificativa, fontes brutas sem página derivada e conceitos recorrentes ainda não documentados.

### Por que isso é importante

Uma wiki mantida por agentes pode crescer rápido, mas também pode se degradar rápido.

Problemas comuns:

- páginas duplicadas;
- links quebrados;
- páginas sem fonte;
- decisões sem contexto;
- páginas desatualizadas;
- documentação que contradiz outra página;
- informações importantes escondidas apenas em raw sources;
- páginas grandes demais;
- conceitos importantes repetidos em vários lugares sem página própria.

O `lint_wiki` deve funcionar como um “revisor arquitetural” da wiki.

### Escopos sugeridos

```text
quick:
  - frontmatter ausente
  - links quebrados
  - páginas órfãs
  - páginas sem atualização recente
  - páginas sem "Veja também"
  - raw sources sem página derivada

full:
  - possíveis contradições
  - conceitos recorrentes sem página própria
  - decisões sem rationale
  - integrações citadas sem página dedicada
  - páginas muito grandes
  - páginas com baixa confiança
  - páginas stale por mudança no código
```

### Exemplo de saída

```markdown
# Relatório de Lint da Wiki

## Links quebrados

- `project-1-arquitetura` aponta para `project-1-deploy`, mas a página não existe.

## Páginas órfãs

- `bug-cdn-spa-access-denied`
- `decisao-mcp-tools`

## Conceitos recorrentes sem página própria

- "vector store por escopo" aparece em 4 páginas, mas não há uma página dedicada.

## Decisões sem rationale

- `decisao-cache-de-sessao`
```

### Prioridade

Alta.

Depois de `index.md` e frontmatter, este é provavelmente o item com maior impacto prático.

---

## ~~4. Criar grafo de links e backlinks~~ ✅ Implementado

> **Status:** Implementado — as tools `wiki_graph`, `backlinks`, `orphans`, `related_pages` e `link_suggestions`. As arestas vêm de links `wiki://page/` no corpo das páginas e do campo `related` do frontmatter; o `wiki_graph` renderiza o grafo nos formatos `summary`, `full` ou `mermaid`. Obs: os links usam a forma `wiki://page/` — a sintaxe estilo Obsidian `[[slug]]` mostrada abaixo não é interpretada.

### Objetivo

Permitir que a wiki seja navegada como um grafo de conhecimento.

As páginas deveriam poder apontar umas para as outras com links no estilo Obsidian:

```markdown
Veja também:
- [[project-1-arquitetura]]
- [[microservice-1-integracao-externa]]
- [[decisao-vetores-por-escopo]]
```

O sistema deve conseguir descobrir:

- quais páginas apontam para uma página;
- quais páginas não recebem nenhum link;
- quais páginas são hubs;
- quais decisões afetam quais serviços;
- quais integrações conectam quais componentes.

### Por que isso é importante

Em projetos de software, o conhecimento não é linear. Uma decisão pode afetar vários serviços; uma integração pode depender de uma decisão transversal; um bug pode revelar um problema de arquitetura.

O grafo ajuda tanto humanos quanto agentes a entender relações.

### Possíveis tools

```text
wiki_graph
backlinks
orphans
related_pages
link_suggestions
```

### Exemplo de uso

```text
backlinks(uri="wiki://page/decisao-vetores-por-escopo")
```

Saída esperada:

```markdown
# Backlinks para `decisao-vetores-por-escopo`

- `project-1-arquitetura`
- `project-1-documentos`
- `project-1-sessao-chat`
```

### Prioridade

Média-alta.

Deve vir depois de `index.md` e frontmatter, porque depende de uma organização mínima da wiki.

---

## ~~5. Introduzir plano de alteração e diff antes de escrita~~ ✅ Implementado

> **Status:** Implementado — `propose_page_update` grava uma proposta revisável em `.advwiki/proposals/<id>.json` e retorna um diff unificado; `apply_page_update` aplica a proposta pelo id, protegendo contra base alterada via verificação de hash MD5 (sobrescrevível com `force`). O modelo de alteração é conteúdo completo proposto + diff; patches estruturados por seção ficam como evolução futura possível.

### Objetivo

Reduzir o risco de o agente degradar páginas existentes usando apenas `append` ou `overwrite`.

Em vez de escrever diretamente, o MCP pode oferecer uma etapa intermediária: propor a alteração antes de aplicá-la.

### Por que isso é importante

`append` é simples, mas pode criar páginas repetitivas e desorganizadas.

`overwrite` é poderoso, mas perigoso.

Um fluxo com plano e diff permite:

- revisar o que será alterado;
- saber quais seções serão tocadas;
- preservar conteúdo existente;
- auditar racional da mudança;
- evitar perda acidental;
- permitir aprovação humana, quando necessário.

### Tools sugeridas

```text
propose_page_update
apply_page_update
```

### Exemplo de plano

```json
{
  "target": "wiki://page/project-1-arquitetura",
  "operation": "patch",
  "reason": "nova decisão sobre escopo dos vector stores",
  "changes": [
    {
      "section": "Decisões Tomadas",
      "action": "add_bullet",
      "content": "Separar vector stores em tenant, projeto e sessão."
    }
  ],
  "affected_links": [
    "project-1-visao-geral",
    "decisao-vetores-por-escopo"
  ]
}
```

### Prioridade

Média-alta.

É especialmente importante quando a wiki começar a ser usada em projetos reais e com páginas longas.

---

## ~~6. Criar tools semânticas de domínio~~ — não será implementado

> **Status:** Não será implementado — contratos rígidos por tipo engessariam um agente capaz (descartando ou espremendo informação que não cabe nos campos fixos) com pouco ganho. O valor pretendido — consistência de formato — é melhor entregue como orientação de estrutura nos skills, com o `lint_wiki` pegando desvios.

### Objetivo

Adicionar tools mais específicas que capturem conhecimento arquitetural de forma estruturada, em vez de depender apenas de `update_page`.

### Por que isso é importante

Tools genéricas são flexíveis, mas exigem muito do agente. Tools semânticas reduzem ambiguidade e melhoram a qualidade do conteúdo gerado.

Em vez de pedir ao agente para criar Markdown livre, o MCP pode oferecer contratos específicos para tipos comuns de conhecimento.

### Tools sugeridas

```text
record_architecture_decision
record_bug_investigation
record_integration_pattern
record_service_overview
record_deployment_note
record_runbook
record_external_dependency
```

### Exemplo: decisão arquitetural

```json
{
  "decision_id": "decisao-vetores-por-escopo",
  "project": "project-1",
  "title": "Separar vector stores por escopo",
  "context": "O produto precisa lidar com documentos compartilhados da organização, arquivos por workspace e anexos por sessão.",
  "decision": "Usar vector stores separados para tenant, workspace e sessão.",
  "alternatives_rejected": [
    "um único vector store global",
    "apenas anexos por sessão"
  ],
  "consequences": [
    "melhora isolamento",
    "aumenta complexidade de roteamento"
  ],
  "related_pages": [
    "project-1-arquitetura",
    "project-1-documentos"
  ]
}
```

### Prioridade

Média.

Muito útil, mas é melhor criar depois de estabilizar o formato das páginas.

---

## 7. Criar fluxo de ingestão curada

### Objetivo

Evoluir a ingestão de conteúdo bruto para um fluxo em que o MCP ajuda a transformar fontes em conhecimento curado.

Hoje, ingerir uma raw source preserva conteúdo, mas não necessariamente atualiza a wiki principal.

A ingestão curada deve:

1. salvar o conteúdo bruto;
2. buscar páginas relacionadas;
3. propor páginas a criar ou alterar;
4. apontar possíveis contradições;
5. atualizar índice e links;
6. registrar no log;
7. rodar lint quick.

### Por que isso é importante

O valor principal da wiki não está em armazenar texto bruto. Está em transformar texto bruto em páginas organizadas, resumidas, linkadas e úteis.

### Tools sugeridas

```text
ingest_extracted_content
propose_ingest_plan
apply_ingest_plan
```

### Exemplo de plano

```markdown
# Plano de Ingestão

## Fonte salva

- `raw://source/session-2026-05-11`

## Páginas relacionadas encontradas

- `project-1-arquitetura`
- `project-1-documentos`
- `decisao-vetores-por-escopo`

## Atualizações propostas

- Criar `project-1-politica-documentos`
- Atualizar `project-1-arquitetura`
- Adicionar backlink em `project-1-visao-geral`

## Possíveis lacunas

- Não existe runbook de reindexação de documentos.
```

### Prioridade

Média.

É uma evolução natural depois das tools semânticas e do diff.

---

## ~~8. Registrar claims rastreáveis~~ ✅ Implementado

> **Status:** Implementado — as tools `find_claims`, `find_claims_without_source`, `find_conflicting_claims` e `verify_claim`, sobre um bloco `## Claims` no corpo da página (labels de campo bilíngues). Obs: o `find_conflicting_claims` é uma heurística de sobreposição de palavras que levanta candidatos a revisão — detecção real de contradição depende da busca semântica do item 11.

### Objetivo

Permitir que afirmações importantes da wiki tenham origem, confiança e data de verificação.

### Por que isso é importante

Arquitetura de software muda. Uma wiki pode ficar obsoleta ou conter afirmações sem lastro.

Claims rastreáveis permitem responder:

- de onde veio essa informação?
- essa afirmação ainda é confiável?
- quando foi verificada pela última vez?
- há outra página dizendo o contrário?
- isso veio de código, log, conversa, documentação ou decisão explícita?

### Exemplo

```markdown
## Claims

- A plataforma usa três escopos mínimos de vector store: tenant, workspace e sessão.
  - Fonte: `wiki://page/decisao-vetores-por-escopo`
  - Confiança: alta
  - Última verificação: 2026-05-11

- O `microservice-2` se comunica com uma API de processamento via gRPC bidirecional.
  - Fonte: `raw://source/session-grpc-microservice-2-api-2026-05-11`
  - Confiança: alta
  - Última verificação: 2026-05-11
```

### Tools sugeridas

```text
find_claims
find_claims_without_source
find_conflicting_claims
verify_claim
```

### Prioridade

Média.

É um recurso avançado, mas muito valioso para evitar “memória alucinada”.

---

## 9. Integrar versionamento Git opcional

### Objetivo

Permitir que a wiki seja versionada com Git.

Como o conhecimento está em Markdown e arquivos locais, Git é uma escolha natural para histórico, diff, rollback e colaboração.

### Por que isso é importante

Uma wiki mantida por agentes precisa ser auditável.

Git oferece:

- histórico de mudanças;
- comparação entre versões;
- rollback;
- branches;
- commits com mensagens semânticas;
- revisão humana via pull request;
- sincronização com repositório remoto.

### Modo sugerido

Adicionar uma flag opcional:

```text
mcp-advwiki --root <PATH> --git
```

### Tools sugeridas

```text
wiki_git_status
wiki_git_diff
wiki_git_commit
wiki_git_history
wiki_git_rollback
```

### Exemplo de commit

```text
docs(project-1): record vector store scoping decision
```

### Prioridade

Média.

Não precisa ser obrigatório na primeira versão, mas é uma excelente opção para uso real.

---

## 10. Detectar obsolescência por referência ao código-fonte

### Objetivo

Permitir que páginas da wiki apontem para arquivos de código e que o sistema detecte quando esses arquivos mudaram.

### Por que isso é importante

Documentação arquitetural fica obsoleta principalmente quando o código muda e a wiki não acompanha.

Se uma página documenta uma classe, módulo ou fluxo, ela pode registrar referências ao código e hashes de última verificação.

### Exemplo de frontmatter

```yaml
code_refs:
  - path: src/microservice_1/handler.rs
    last_seen_hash: abc123
  - path: src/project_1/schema.rs
    last_seen_hash: def456
```

### Exemplo de lint

```markdown
# Páginas possivelmente obsoletas

- `flow-microservice-1-handler`
  - Referencia `handler.rs`
  - O arquivo mudou desde a última verificação.
```

### Tools sugeridas

```text
scan_code_refs
mark_code_refs_verified
find_stale_code_refs
```

### Prioridade

Média.

Para uma wiki arquitetural de software, este recurso pode se tornar um grande diferencial.

---

## 11. Preparar busca híbrida opcional

### Objetivo

Preparar o mecanismo de busca para suportar, no futuro, modos além do BM25.

O BM25 é simples, rápido e excelente para termos exatos. Mas, com uma wiki grande, pode ser útil adicionar busca semântica e reranking.

### Por que isso é importante

A wiki pode crescer e as perguntas podem ficar mais semânticas:

- “como lidamos com falha temporária de integração?”
- “qual decisão explica esse padrão de retry?”
- “onde documentamos isolamento de tenant?”

Essas perguntas nem sempre usam os mesmos termos que as páginas.

### Modos sugeridos

```rust
enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}
```

### Tool sugerida

```json
{
  "question": "como funciona o fluxo de upload de documentos?",
  "mode": "hybrid",
  "maxPages": 8
}
```

### Prioridade

Baixa-média.

Não deve ser prioridade antes de melhorar a estrutura da wiki. Uma wiki bem organizada com BM25 pode ir longe.

---

## 12. Criar bootstrap de sessão mais inteligente

### Objetivo

Oferecer uma tool dedicada para recuperar o contexto inicial de um projeto ou serviço.

Em vez de o agente chamar várias buscas soltas, ele poderia chamar:

```text
bootstrap_context(project="project-1")
```

### Por que isso é importante

Ao iniciar uma conversa sobre um projeto, o agente precisa saber:

- quais páginas centrais existem;
- quais decisões recentes foram registradas;
- quais lacunas são conhecidas;
- quais páginas estão stale;
- quais serviços ou módulos são relevantes.

### Exemplo de saída

```markdown
# Contexto inicial — `project-1`

## Páginas centrais

- `project-1-visao-geral`
- `project-1-arquitetura`
- `microservice-1-integracao-externa`

## Atualizações recentes

- 2026-05-11: registrada decisão sobre vector stores.
- 2026-05-10: documentado problema de CDN para SPA.

## Possíveis lacunas

- Não há página sobre política de versionamento de documentos.
- Não há runbook completo de deploy.
```

### Prioridade

Média-alta.

Depois do `index.md`, este recurso melhora muito o uso cotidiano da skill.

---

## 13. Criar modo de revisão arquitetural

### Objetivo

Adicionar uma capacidade de diagnóstico da própria wiki/projeto.

A tool não responderia uma pergunta específica. Ela revisaria a memória arquitetural e sugeriria melhorias.

### Por que isso é importante

Com o tempo, a wiki pode revelar lacunas no próprio projeto:

- decisões implícitas ainda não registradas;
- riscos recorrentes;
- falta de runbooks;
- integrações sem contrato documentado;
- áreas com documentação fraca;
- padrões usados mas não nomeados.

### Tool sugerida

```text
review_project_memory
```

### Exemplo de saída

```markdown
# Diagnóstico Arquitetural — `project-1`

## Decisões bem documentadas

- Vector stores por escopo
- Comunicação gRPC com API de processamento

## Decisões implícitas, mas não registradas

- Estratégia de isolamento multi-tenant
- Política de retenção de sessões e anexos

## Riscos arquiteturais

- Falta runbook de recuperação de falha no canal gRPC.
- Falta documentação de limites dos anexos por sessão.

## Próximas páginas recomendadas

- `project-1-runbook-grpc-microservice-1`
- `project-1-politica-retencao-documentos`
- `decisao-isolamento-multitenant`
```

### Prioridade

Média.

Pode virar um dos recursos mais interessantes do projeto, especialmente para uso por arquitetos e tech leads.

---

# Ordem sugerida de implementação

## Fase 1 — Fundação da wiki

1. ~~Criar `index.md` navegável.~~ ✅
2. ~~Adicionar YAML frontmatter.~~ ✅
3. ~~Melhorar `lint_wiki` para links, órfãos e metadados.~~ ✅
4. ~~Criar backlinks/grafo básico.~~ ✅

## Fase 2 — Escrita mais segura

5. ~~Criar plano de alteração + diff.~~ ✅
6. ~~Criar tools semânticas de domínio.~~ — não será implementado
7. Criar fluxo de ingestão curada.

## Fase 3 — Qualidade e auditoria

8. ~~Registrar claims rastreáveis.~~ ✅
9. Integrar Git opcional.
10. Detectar obsolescência por referência ao código.

## Fase 4 — Inteligência de uso

11. Preparar busca híbrida opcional.
12. Criar bootstrap de sessão.
13. Criar modo de revisão arquitetural.

---

# Critério de sucesso

O `mcp-advwiki` deve evoluir para um sistema em que o agente consiga:

- descobrir rapidamente o que já se sabe sobre um projeto;
- responder usando memória persistente sem inventar contexto;
- registrar novas decisões de forma estruturada;
- preservar fontes brutas sem misturá-las com conhecimento curado;
- manter links e índice navegáveis;
- detectar documentação obsoleta;
- revisar a qualidade da própria wiki;
- permitir auditoria das mudanças feitas pelo agente.

Em resumo: o objetivo não é apenas ter busca local. O objetivo é criar uma **memória arquitetural persistente, navegável, auditável e incremental** para projetos de software.
