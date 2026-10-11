# Desenvolvimento do BigSound

- `main`: versões examinadas e integradas.
- `dev-xathay`: trabalho em andamento e correções; a CI roda a cada push.
- Integração: pull request de `dev-xathay` para `main`, com diff revisado e CI aprovada.

```bash
git switch dev-xathay
git pull --ff-only
```

Use o checkout existente para trabalhar; a pasta de publicação sem `.git`
é uma exportação para um novo repositório, não o diretório de desenvolvimento.
Não faça push de alterações em andamento diretamente para `main`.

Antes de enviar, execute as verificações de [VALIDATION.md](docs/VALIDATION.md),
revise código, imagens e anexos e confira o nome/email públicos do commit.
A varredura inclui autores, committers, mensagens e blobs alcançáveis.
Não inclua trailers automáticos de coautoria por IA. As identidades públicas
do Git são definidas por quem mantém e confirma as alterações.

O GitRepo pode abrir o checkout existente e auxiliar nas operações Git.
Confira a branch selecionada e o diff antes de confirmar commit/push.
A interface Git não substitui os testes nem a revisão do PR.
