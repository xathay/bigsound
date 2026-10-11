# Preparação para o BigCommunity

Primeiro atualize `xathay/bigsound`, confira o README renderizado e acompanhe
as Actions. Corrija falhas antes de criar o repositório no `big-comm`.
A aprovação de uma release de áudio exige também escuta e hardware alvo;
publicar fontes para revisão não certifica esses testes físicos.

## Envio pelo repositório pessoal

1. Confira o README, as capturas, a autoria e o relatório de validação.
2. Aguarde os dois jobs da CI: build/validação e auditoria RustSec.
3. Crie o repositório de destino no BigCommunity após essa revisão.
4. Se enviar pelo checkout existente, preserve o histórico revisado e configure
   o destino explicitamente. Revise o diff e anexos antes de cada envio.
5. Não adicione trailers automáticos de coautoria por IA.

## Alternativa: pacote limpo de fontes

A pasta `bigsound` do pacote preparado contém somente fontes, documentação,
licença e imagens revisadas. Não inclui `.git`, histórico, binários locais,
logs, estudos de ícone, dados de usuário ou credenciais.

Se optar por um repositório com histórico novo, envie o conteúdo dessa pasta,
incluindo `.github` e `.gitignore`. Confira as Actions após o envio.
Não envie a pasta `release-review` inteira.

Não publique o antigo pacote binário local. Construa um novo pacote em ambiente
limpo da distribuição. `RELEASE-REVIEW.md` distingue testes realizados e lacunas.
