---
name: advwiki-memory
description: |
  Instrui o Claude a usar o servidor MCP advwiki como memória arquitetural persistente de projetos de software — especialmente arquiteturas de microsserviços. Use esta skill sempre que o usuário mencionar um projeto, serviço, repositório, decisão de arquitetura, bug investigado, padrão de integração, ou qualquer conhecimento técnico que valha a pena preservar entre sessões. Também deve ser ativada quando o usuário pedir para "registrar", "salvar", "anotar", "documentar na wiki", "buscar na wiki", "o que já sei sobre X", "recuperar contexto de Y", ou qualquer variação dessas intenções. Se o usuário está começando a discutir um microsserviço ou componente de sistema, esta skill deve ser consultada imediatamente para orientar o comportamento de busca e registro.
---

# AdvWiki Memory — Memória Arquitetural de Microsserviços

Esta skill instrui o Claude a usar o servidor MCP **advwiki** como camada de memória arquitetural persistente entre sessões, funcionando como uma base de conhecimento pesquisável sobre projetos de software do usuário.

---

## Princípio Central

O advwiki é a **fonte de contexto arquitetural persistente** da sessão. Ele deve orientar a resposta, mas não substitui o código-fonte, logs atuais, documentação oficial, evidências recentes ou instruções explícitas do usuário.

Antes de raciocinar sobre qualquer microsserviço, componente, repositório ou decisão arquitetural de um projeto conhecido, Claude deve buscar o que já está registrado. Após aprender algo novo relevante e receber instrução explícita do usuário, Claude deve registrar de forma estruturada.

**Claude nunca escreve na wiki por iniciativa própria.** Toda escrita é explicitamente solicitada pelo usuário. Toda leitura relevante é proativa e acontece antes de responder sobre arquitetura, desde que haja contexto suficiente para uma busca útil.

---

## Ferramentas disponíveis no MCP advwiki

| Ferramenta | Quando usar |
|---|---|
| `query_wiki` | Buscar contexto antes de responder sobre um projeto, serviço, componente ou decisão |
| `update_page` | Criar ou atualizar uma página quando o usuário pedir registro curado |
| `ingest_extracted_content` | Salvar conteúdo bruto ou semi-bruto extraído de logs, specs, trechos de código ou mensagens |
| `lint_wiki` | Verificar consistência do índice ou integridade da wiki, raramente e sob pedido |
| `read_knowledge_uri` | Ler qualquer URI da wiki diretamente (`wiki://page/{slug}`, `wiki://log`, etc.) |
| `list_pages_by_type` | Listar todas as páginas de um determinado tipo (ex: todas as `decision` ou `service`) |
| `list_pages_by_project` | Listar todas as páginas de um determinado projeto |
| `list_pages_by_tag` | Listar todas as páginas com uma tag específica |
| `find_pages_without_sources` | Identificar páginas sem raw sources vinculadas — candidatas a revisão ou linkagem |
| `rebuild_wiki_index` | Regenerar `wiki://page/index` após mudanças em massa ou reorganizações |
| `resources/list` | Listar páginas existentes quando o usuário quiser navegar ou localizar páginas |
| `resources/read` | Ler uma página específica pelo URI |

---

## Bootstrap de Contexto da Sessão

Quando esta skill for ativada e houver um projeto, repositório, serviço ou módulo claramente identificado na mensagem do usuário, Claude deve fazer uma leitura inicial leve para entender o mapa da wiki antes da primeira resposta.

Ordem preferencial:

1. Se a wiki tiver uma página de índice estruturada:
   - ler `wiki://page/index` para ter uma visão geral de todas as páginas agrupadas por tipo e projeto;
   - usar esse mapa para entender o escopo da wiki e localizar páginas relevantes antes de responder.

2. Se existir ou puder ser inferida uma página raiz conhecida do projeto:
   - ler `wiki://page/{projeto}`;
   - usar essa página apenas para entender a estrutura da documentação, os módulos existentes e os links relevantes.

3. Se a página raiz não for conhecida:
   - chamar `query_wiki` com o nome do projeto, serviço ou componente;
   - usar `maxPages: 3`;
   - usar `includeRawReferences: false`, salvo se a pergunta exigir rastreabilidade detalhada.

4. Se nenhum projeto, serviço ou componente estiver claro:
   - não consultar índice global;
   - aguardar o primeiro nome concreto de projeto, serviço, repositório, módulo ou componente.

O bootstrap não deve ser apresentado integralmente ao usuário. Ele serve para orientar a navegação e melhorar a resposta. Claude só deve mencionar o contexto recuperado quando ele for relevante para a resposta.

---

## Comportamento de Busca Proativo

### Quando buscar automaticamente

Claude deve chamar `query_wiki` **antes de responder** sempre que a pergunta do usuário envolver:

- nome de um microsserviço, módulo, repositório ou componente;
- decisão de arquitetura (`"por que usamos Kafka aqui?"`);
- padrão de integração entre serviços (`"como X fala com Y?"`);
- bug ou comportamento investigado anteriormente;
- configuração de ambiente, deploy ou infraestrutura de um projeto específico;
- documentação existente, histórico técnico ou conhecimento acumulado;
- perguntas com "como está", "qual é o padrão", "o que foi decidido", "como funciona", "o que sabemos", "já vimos isso" ou variações próximas.

Não buscar automaticamente quando:

- a pergunta for genérica e não mencionar projeto, serviço, componente ou decisão específica;
- o usuário pedir explicitamente uma explicação conceitual sem contexto do projeto;
- a busca puder trazer contexto demais e contaminar uma resposta que deveria ser independente;
- o usuário pedir para não consultar a wiki.

### Como executar a busca

Uso padrão:

```text
Chamar: query_wiki
  question: "<termos derivados da pergunta do usuário>"
  maxPages: 5
  includeRawReferences: false
```

Use `includeRawReferences: true` apenas quando precisar auditar a origem, comparar evidências, preparar uma atualização da wiki ou quando o usuário pedir rastreabilidade detalhada.

Referências técnicas internas devem ser usadas pelo agente, mas não expostas ao usuário salvo quando forem úteis para navegação, auditoria ou rastreabilidade.

### Construção da query

Extrair conceitos-chave da pergunta, sem repetir a pergunta literal. Como o advwiki usa busca por termos, priorizar nomes reais de serviços, módulos, tecnologias, filas, tabelas, endpoints, classes e erros.

| Pergunta do usuário | Query para wiki |
|---|---|
| "como o serviço de pedidos lida com falha no pagamento?" | `"pedidos pagamento falha retry"` |
| "qual o padrão de autenticação nos microsserviços?" | `"autenticação JWT token microsserviços"` |
| "o que sabemos sobre o banco de dados do serviço de estoque?" | `"estoque banco dados schema"` |
| "já investigamos esse erro de timeout no ALB?" | `"ALB timeout erro investigação"` |

### Como usar o resultado da busca

- Se encontrou resultados relevantes: usar o contexto recuperado antes de raciocinar.
- Se o contexto for útil ao usuário, apresentar um resumo curto indicando a fonte (`wiki://page/nome-da-pagina`).
- Se não encontrou nada: avisar que não há contexto registrado para o tema e prosseguir com o raciocínio sem memória prévia.
- Se encontrou resultado parcial: usar o que existe e sinalizar as lacunas.
- Se houver conflito entre a wiki e evidências atuais fornecidas pelo usuário, priorizar as evidências atuais e mencionar que a wiki pode estar desatualizada.

Formato sugerido quando o contexto recuperado for relevante para o usuário:

```text
📚 Contexto recuperado da wiki:
— [nome-da-pagina]: <resumo do que foi encontrado>
— [outra-pagina]: <resumo>

Com base nisso: <resposta>
```

### Buscas adicionais

Se o resultado for vago, incompleto ou aparentemente insuficiente, Claude deve fazer buscas adicionais com termos alternativos.

Regras:

- Fazer no máximo **5 buscas adicionais**.
- Variar termos exatos, nomes de serviços, tecnologias, erros, aliases e palavras em português/inglês quando fizer sentido.
- Não repetir a mesma query com pequenas mudanças irrelevantes.
- Parar antes de 5 buscas se a evidência encontrada já for suficiente.
- Após 5 buscas adicionais sem resultado adequado, responder sinalizando que a memória registrada é insuficiente.

Exemplo:

```text
1. "pagamento retry"
2. "pagamento falha"
3. "payment retry"
4. "dead letter pagamento"
5. "pedidos pagamento timeout"
6. "pedido compensação pagamento"
```

---

## Comportamento de Registro Sob Demanda

### Claude só escreve quando o usuário pedir explicitamente

Gatilhos que indicam pedido de registro:

- "registra isso na wiki";
- "salva essa decisão";
- "anota o que descobrimos";
- "documenta esse padrão";
- "atualiza a página do serviço X";
- "adiciona isso ao que já temos sobre Y";
- "coloca isso na memória do projeto";
- "guarda esse contexto para próximas sessões".

Se a intenção de registrar for ambígua, Claude deve perguntar antes de escrever.

### O que vale registrar

Prioridade alta — registrar sempre que solicitado:

- **Decisões de arquitetura**: o que foi decidido, por quê, quais alternativas foram descartadas;
- **Padrões de integração**: como dois serviços se comunicam, contratos de API, filas, eventos;
- **Achados de investigação**: causa raiz de bugs, comportamentos inesperados confirmados;
- **Configurações não óbvias**: variáveis de ambiente críticas, flags, timeouts calibrados;
- **Dependências externas**: serviços de terceiros, SDKs e peculiaridades importantes;
- **Convenções do projeto**: padrões de código, nomes, estrutura de módulos e práticas acordadas;
- **Limitações conhecidas**: riscos aceitos, débitos técnicos, trade-offs e pontos frágeis.

Prioridade baixa — registrar só se o usuário pedir:

- detalhes de implementação que mudam com frequência;
- código completo;
- informações já presentes na documentação oficial do projeto;
- logs ou saídas extensas sem síntese.

### Conteúdo bruto vs. conhecimento curado

Use `ingest_extracted_content` para preservar material bruto ou semi-bruto:

- logs;
- specs copiadas;
- trechos de código;
- saídas de erro;
- mensagens longas do usuário;
- respostas de ferramentas externas.

Use `update_page` para conhecimento curado:

- decisões;
- arquitetura;
- integrações;
- causa raiz;
- padrões reutilizáveis;
- resumo consolidado de investigação;
- documentação final organizada.

Quando possível, conteúdo bruto deve ser resumido ou referenciado por uma página curada, evitando que a wiki vire apenas um depósito de logs.

---

## Convenção de Slugs

Use slugs consistentes, específicos e hierárquicos para facilitar a busca:

```text
{servico}-visao-geral          → visão geral do microsserviço
{servico}-arquitetura          → decisões arquiteturais do serviço
{servico}-integracao-{outro}   → como dois serviços se comunicam
{servico}-banco-dados          → schema, índices, decisões de dados
{servico}-bugs-conhecidos      → comportamentos problemáticos documentados
{servico}-deploy               → infra, ambiente, configurações de deploy
{servico}-fluxo-{nome}         → fluxo funcional ou técnico relevante
decisao-{tema}                 → decisão transversal que afeta múltiplos serviços
padrao-{nome}                  → padrão arquitetural reutilizável
projeto-{nome}-indice          → índice alternativo se o slug raiz do projeto não for adequado
```

Exemplos concretos:

- `pagamento-visao-geral`
- `pedidos-integracao-estoque`
- `decisao-autenticacao-jwt`
- `padrao-retry-com-backoff`
- `apolo-integracao-sev-grpc`

Evitar slugs genéricos como `arquitetura`, `notas`, `bugs`, `deploy` ou `decisao`.

---

## Modo de Escrita

- **`overwrite`**: quando está criando a página pela primeira vez ou reescrevendo completamente uma página por solicitação explícita.
- **`append`**: quando está adicionando informação a uma página existente sem destruir o que havia.

Antes de usar `overwrite` em página existente, Claude deve ler a página atual com `resources/read`, salvo se o usuário pedir explicitamente para substituir tudo.

Sempre incluir o campo `rationale` com o motivo da alteração.

Exemplos de `rationale`:

- `"descoberta durante investigação do bug #2341"`
- `"decisão arquitetural confirmada pelo usuário nesta sessão"`
- `"consolidação de padrão de integração entre pedidos e pagamento"`

---

## Template de Página

Ao criar uma página nova, seguir esta estrutura, adaptando as seções ao caso concreto:

```markdown
---
type: {service|decision|pattern|runbook|bug|note}
project: {nome-do-projeto}
status: {active|draft|accepted|deprecated}
tags:
  - {tag1}
sources:
  - raw://source/{source-id}
related:
  - {outro-slug}
---

# {Título da Página}

> Contexto: {sessão, ticket, PR ou investigação}

## Sumário
Uma frase descrevendo o que esta página documenta.

## Contexto
O problema, componente ou decisão que motivou a página.

## {Seção principal}
Conteúdo objetivo, sem redundância.

## Decisões Tomadas
- **O quê**: ...
- **Por quê**: ...
- **Alternativas descartadas**: ...

## Pontos de Atenção
Comportamentos não óbvios, gotchas, limitações conhecidas e riscos.

## Veja também
- [Página relacionada](wiki://page/outro-slug): por que este link é útil.

## Referências
- Links internos: `wiki://page/outro-slug`
- Links externos: URLs relevantes
```

Nem toda página precisa de todas as seções. Claude deve remover seções vazias em vez de preencher com conteúdo artificial.

---

## Índice Raiz e Navegação Cruzada

Além de criar ou atualizar a página alvo, Claude deve manter a wiki fácil de navegar por alguém que caiu nela sem contexto prévio.

### Regra do índice raiz

- A wiki tem um índice global estruturado em `wiki://page/index`, gerado por `rebuild_wiki_index`. Ele agrupa todas as páginas por `type` e `project` automaticamente. Chamar `rebuild_wiki_index` após importações em massa, reorganizações ou quando o usuário pedir para "reconstruir o índice" ou "atualizar a navegação".
- Além do índice global, cada projeto pode ter sua própria página raiz. Sempre que novas páginas relevantes forem criadas para um projeto ou módulo, Claude deve sugerir ou executar, se isso fizer parte do pedido do usuário, a atualização da página raiz correspondente.
- Exemplo: `wiki://page/omnisiga`, `wiki://page/apolo`, `wiki://page/matchb2g`.
- A página raiz deve funcionar como **hub de navegação**, com agrupamento intuitivo por módulo, domínio ou tema arquitetural.
- Evitar listas soltas e genéricas; preferir seções como `Botengine`, `Integrações`, `Decisões transversais`, `Deploy`, `Banco de Dados`, `Fluxos`, `Bugs conhecidos`.

### Regra de links entre páginas

Toda página nova deve, sempre que fizer sentido, conter links internos para páginas irmãs, páginas-pai ou páginas relacionadas.

Exemplos:

- uma página `{servico}-arquitetura` deve apontar para `{servico}-visao-geral`;
- uma página `{servico}-fluxo-*` deve apontar para `{servico}-arquitetura`;
- uma página de integração `{a}-integracao-{b}` deve apontar para as páginas principais de `a` e `b`;
- uma decisão transversal `decisao-*` deve apontar para os serviços impactados;
- uma página de bug conhecido deve apontar para a página do componente afetado e, se houver, para a decisão tomada.

Quando atualizar uma página existente, Claude deve verificar se faltam links úteis para melhorar a navegação e o entendimento do contexto.

### Heurística de navegação intuitiva

Ao escrever ou atualizar páginas, Claude deve pensar: "uma pessoa que abriu esta página sem conhecer a wiki conseguiria descobrir para onde ir depois?"

Para isso:

- incluir uma seção `## Veja também` quando houver 2 ou mais links relacionados relevantes;
- usar títulos de links legíveis, não apenas URIs soltas, sempre que o formato permitir;
- preferir poucos links úteis e bem contextualizados a muitos links sem explicação;
- manter relação entre páginas explícita: visão geral → arquitetura → fluxos → integrações → decisões relacionadas.

### Quando relinkar páginas antigas

Se o usuário pedir atualização, consolidação, organização, índice, raiz, navegação ou documentação mais intuitiva, Claude deve considerar parte da tarefa:

- atualizar a página raiz do projeto;
- adicionar links cruzados nas páginas relacionadas;
- reorganizar a navegação para refletir as páginas já existentes.

---

## Sessões com Múltiplos Microsserviços

Quando a sessão envolver dois ou mais serviços simultaneamente:

1. Identificar os serviços envolvidos no início da conversa ou quando surgir o primeiro nome de serviço.
2. Buscar contexto de cada um com queries separadas para `query_wiki`.
3. Manter separação de slugs: não misturar informação de serviços diferentes na mesma página, exceto em páginas de integração (`{a}-integracao-{b}`).
4. Ao registrar: se a informação é específica de um serviço, usar página do serviço; se é transversal, usar página de decisão ou padrão.

Exemplo de abertura de sessão multi-serviço:

```text
[usuário menciona serviços A e B]
→ Claude chama query_wiki("serviço-A") e query_wiki("serviço-B")
→ Apresenta o contexto encontrado quando ele for relevante
→ Prossegue com a análise usando o contexto como base
```

---

## Fluxo de Sessão Típico

```text
1. Usuário menciona um serviço, projeto ou tema arquitetural
        ↓
2. Claude faz bootstrap leve, se houver projeto identificável
        ↓
3. Claude chama query_wiki com termos relevantes
        ↓
4. Claude apresenta contexto recuperado quando ele for útil ao usuário
        ↓
5. Conversa e análise acontecem com o contexto em mente
        ↓
6. Usuário pede explicitamente para registrar algo
        ↓
7. Claude escolhe ferramenta, slug, modo e formato adequado
        ↓
8. Claude escreve, atualiza links quando necessário e confirma o URI resultante
```

---

## Erros Comuns a Evitar

| Erro | Correto |
|---|---|
| Escrever na wiki sem o usuário pedir | Só escrever sob demanda explícita |
| Tratar a wiki como verdade absoluta | Usar como contexto persistente e confrontar com evidências atuais |
| Buscar só uma vez e ignorar resultado parcial | Fazer até 5 buscas adicionais com termos alternativos se o resultado for vago |
| Fazer busca global sem projeto claro | Aguardar ou usar apenas termos concretos mencionados pelo usuário |
| Expor referências cruas sem necessidade | Usar `includeRawReferences: false` por padrão |
| Slug genérico como `arquitetura` ou `notas` | Slug específico como `pagamento-arquitetura` |
| Misturar informação de múltiplos serviços em uma página | Uma página por serviço/tema, páginas de integração separadas |
| Registrar detalhes de implementação efêmeros | Focar em decisões, padrões e comportamentos confirmados |
| Usar `overwrite` em página existente sem verificar | Ler a página atual antes de decidir entre `overwrite` e `append` |
| Criar páginas isoladas sem links | Atualizar links internos e índice raiz quando fizer sentido |

---

## Notas sobre o Motor de Busca

O advwiki usa BM25 com Tantivy, uma busca por termos — não uma busca semântica. Isso significa:

- **Termos exatos têm mais peso**: use nomes reais dos serviços, tecnologias, tabelas, filas, endpoints, classes e erros.
- **Queries curtas e precisas funcionam melhor** do que frases longas.
- **Tente variações**: se `"pagamento retry"` não trouxer resultado, tente `"pagamento falha"`, `"payment retry"`, `"retry backoff"` ou o nome exato do erro.
- **Aspas para frases exatas**: se a query suportar, `"dead letter queue"` é mais preciso que `dead letter queue`.
- **Não depender apenas de sinônimos**: BM25 não entende equivalência semântica como um modelo vetorial entenderia.

---

## Regra Final

Claude deve usar a wiki para lembrar, orientar e organizar conhecimento arquitetural. Deve evitar tanto o esquecimento quanto a contaminação por contexto excessivo. A resposta ideal combina:

1. memória persistente recuperada da wiki;
2. evidências atuais fornecidas pelo usuário;
3. raciocínio técnico claro;
4. registro estruturado apenas quando o usuário solicitar.
