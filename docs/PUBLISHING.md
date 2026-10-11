# Preparação para o BigCommunity

Trabalhe em `dev-xathay` e abra um pull request para `main` em `xathay/bigsound`.
Confira o README renderizado na branch e acompanhe
as Actions. Corrija falhas antes de criar o repositório no `big-comm`.
A aprovação de uma release de áudio exige também escuta e hardware alvo;
publicar fontes para revisão não certifica esses testes físicos.

## Envio pelo repositório pessoal

1. Confira o README, as capturas, a autoria e o relatório de validação.
2. Aguarde os dois jobs da CI: build/validação e auditoria RustSec.
3. Crie um repositório novo no BigCommunity após essa revisão.
4. Para começar com atribuição independente de contribuidores antigos, use o
   pacote limpo de fontes e inicialize um histórico novo com sua identidade.
   Não use fork, importação ou mirror do histórico anterior para essa finalidade.
5. Revise os anexos e não adicione trailers automáticos de coautoria por IA.

## Alternativa: pacote limpo de fontes

A pasta `bigsound` do pacote preparado contém somente fontes, documentação,
licença e imagens revisadas. Não inclui `.git`, histórico, binários locais,
logs, estudos de ícone, dados de usuário ou credenciais.

Se optar por um repositório com histórico novo, envie o conteúdo dessa pasta,
incluindo `.github` e `.gitignore`. Confira as Actions após o envio.
Não envie a pasta `release-review` inteira.

Não publique o antigo pacote binário local. Construa um novo pacote em ambiente
limpo da distribuição. `RELEASE-REVIEW.md` distingue testes realizados e lacunas.
