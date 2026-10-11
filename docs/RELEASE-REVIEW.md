# BigSound 0.7.2 — revisão para envio

Data: 11/10/2026. Base: `xathay/bigsound`, commit
`b8a875bfaba1e783dc64715efebae837e0fa9abf`.

**Situação:** fontes preparadas para revisão no BigCommunity; validação
automatizada local concluída. A atualização do repositório pessoal precede
o envio ao BigCommunity. A aprovação
de uma release de áudio exige escuta/hardware alvo e execução da nova CI.
A varredura de sigilo complementa a conferência humana do pacote final.

## Correções

- Corrigida a recuperação de ganho dos limiters: o ganho recuperado continua
  limitado pelo pico da amostra corrente. O novo teste falhava antes da correção.
- Protegidos os cinco módulos contra NaN/Inf, entradas anormalmente grandes,
  controles inválidos e taxas não suportadas. Frequências de filtro ficam
  estritamente abaixo de Nyquist, inclusive nas taxas baixas.
- Ganho de compensação do BigBass calculado na atualização de parâmetros,
  evitando uma exponenciação por amostra e canal.
- Controles estéreo mantidos juntos e atualizações/retentativas serializadas.
  Perfis são enviados em lotes limitados para caber no buffer de 1 KiB do
  `pw-cli` antigo. A falha de um lote restaura os controles confirmados; se
  a restauração também falhar, somente o estado anterior é agendado para retry.
  Não há garantia de troca simultânea de todo o perfil num único frame.
  Limite conferido no [código oficial do pw-cli 1.0.5](https://github.com/PipeWire/pipewire/blob/1.0.5/src/tools/pw-cli.c).
- Subprocessos limitados a dois segundos e 4 MiB por saída; testes incluem
  processo travado, descendente mantendo pipe aberto e saída excessiva.
- Persistência atômica com permissão 0600 e sincronização; leitura limitada de
  arquivos regulares, sem seguir symlinks. Perfis têm limite de 64 KiB.
- Identidade do nó inclui `object.serial`, detectando também reutilização do
  mesmo id após recriar a cadeia. Mantida a correção local preexistente de
  movimentação e conferência do roteamento fixado.
- Interface dividida em Principal e Avançado. A primeira oferece perfil,
  destino, quatro ajustes rápidos e cinco barras dos controles reais; a segunda
  expõe todos os 17 parâmetros. As barras não representam espectro, resposta
  em frequência, loudness ou áudio medido.
- Fila limitada de comandos fora da thread GTK, com coalescência de sliders e
  preservação da ordem entre alterações de perfil/saída. Erros são exibidos;
  controles retornam ao estado confirmado, inclusive se o daemon ficar offline.
  Fechar a janela aguarda comandos já aceitos; uma falha cancela o fechamento.
- Restauração reaplica o perfil inteiro. Atualização periódica acompanha perfil,
  parâmetros e dispositivos externos. Atalhos Ctrl+1/Ctrl+2, rótulos acessíveis,
  tradução brasileira, porcentagens e unidades coerentes nas duas abas.
- Atualizados `anyhow` 1.0.104, `event-listener` 5.4.2 e `gettext-rs` 0.8.0
  após alertas RustSec de segurança de memória
  ([anyhow](https://rustsec.org/advisories/RUSTSEC-2026-0190.html),
  [event-listener](https://rustsec.org/advisories/RUSTSEC-2026-0221.html),
  [gettext-rs](https://rustsec.org/advisories/RUSTSEC-2026-0244.html)). Auditoria final: zero
  vulnerabilidades e zero alertas, sem exclusões de advisories.
- Ícone aprovado de alto-falante com assinatura Big, colorido e simbólico em SVG autocontido;
  exportação PNG 512 px sem metadados pessoais. Capturas reais em português, temas claro/escuro e
  escala nativa 2×, feitas com GTK/X11, D-Bus e PipeWire privados. Documentação
  da direção em [ICON-DESIGN.md](ICON-DESIGN.md).
- Corrigidas afirmações sobre true peak, aliasing, Atmos, codecs e filtros;
  descrições dos perfis e traduções agora refletem o processamento implementado.
- Build do pacote limpa os artefatos dos membros do workspace antes de
  compilar. Um teste do pacote detectou cache de daemon antigo após extração
  de fonte com timestamps normalizados; a correção evita esse falso build.
- Remapeados caminhos de compilação/Cargo: a varredura dos binários
  encontrou caminhos pessoais em mensagens de panic mesmo após strip.
  A receita preserva flags do builder e remove esses caminhos do binário.
- Lockfile preservado nas instalações/receitas; licença completa no pacote,
  dependências diretas explícitas e removido conflito artificial com outros
  processadores de áudio. Comparar efeitos em cascata continua sendo necessário.
- CI com permissões de leitura, Actions fixadas em commits, limite de duração,
  testes DSP/ABI/sessão e job RustSec que rejeita inclusive warnings.

## Evidências locais

| Verificação | Resultado |
|---|---|
| Formatação e Clippy com `-D warnings` | aprovados |
| Testes Rust | 77 aprovados, zero falhas |
| Cadeia DSP | 15 perfis × oito taxas de 8 a 384 kHz |
| Alocação no processamento/atualização/reset | zero após construção |
| ABI LADSPA dos cinco `.so` | 900 cenários aprovados |
| Sessão privada D-Bus/PipeWire | 17 controles reais em todos os perfis, persistência, rejeição de entradas, 40 escritas concorrentes, rollback de escrita falha, rollback de perfil parcialmente aplicado e reinício aprovados |
| Captura de áudio da cadeia real | 262.144 frames estéreo finitos na repetição final; pico 0,9440609, fluxo virtual |
| GTK4 | smoke Broadway e capturas X11 2×; alternância de abas, sincronização de perfil e interação real de slider conferidas; rollback com cadeia indisponível conferido |
| ShellCheck, sintaxe Bash, gettext e desktop entry | aprovados |
| Build atual | release Rust com lockfile; fontes para GitHub, sem binários locais no pacote de publicação |
| RustSec | base atualizada: 1.296 advisories, 205 dependências; zero vulnerabilidades/alertas |
| Sigilo do histórico base | 33 commits e 163 blobs alcançáveis examinados, sem achados pelos padrões do scanner |
| Coautoria por IA | zero trailers no histórico local e remoto consultado; nenhum trailer adicionado |
| Imagens | três telas em dois temas e ícone; sessão isolada, PNGs sem EXIF ou campos textuais |

A primeira execução da atualização aprovou RustSec, testes Rust e ABI, mas
falhou na validação de perfil contra o PipeWire antigo:
[execução 38112381410](https://github.com/xathay/bigsound/actions/runs/38112381410).
O diagnóstico posterior mostrou controles inalterados após um envio grande;
a correção na branch de desenvolvimento limita os lotes e testa rollback com falha injetada
no segundo lote. Consulte os checks do PR para o resultado da rodada atual.

## Estresse e performance

Foram processados **684.180.352 frames estéreo**, equivalentes a 600 segundos
por combinação de quatro taxas e três blocos (duas horas somadas de áudio
simulado), com quatro processos de carga de CPU. Tempo de parede: 142,18 s.
Pico de RSS do processo DSP: 20.800 KiB. Nenhuma falha, amostra inválida ou
violação do teto da cadeia DSP foi detectada.

Em 48 kHz/blocos de 1024, p99 de 200,445 µs e máximo de 1.185,555 µs,
contra 21.333 µs de prazo de bloco; throughput de 123,104 vezes tempo real.
A faixa de throughput no ensaio foi 23,495–134,016 vezes tempo real.
Compilações também ocorreram no ambiente durante parte do ensaio: os
máximos incluem contenção e preempção do escalonador. Em alguns cenários
com blocos pequenos/taxas altas houve máximos acima do prazo nominal.
Isso impede afirmar garantia de hard real time apenas pelo benchmark.

Ambiente: x86_64, Rust 1.98.1, PipeWire 1.6.8, GTK4 4.22.4 e libadwaita
1.9.3. Resultados não são automaticamente transferíveis a outra máquina,
versão da distribuição ou política WirePlumber.

## Pendências e débitos técnicos

1. **Antes da release de áudio:** revisar o diff e anexos; testar com fones/alto-falantes
   em volume moderado e A/B com loudness equivalente. Validar Bluetooth,
   HDMI, hotplug, suspensão/retomada e roteamento fixado/automático no
   hardware e na versão BigCommunity de destino. A sessão privada não usa
   dispositivos físicos nem política WirePlumber.
2. **Antes de distribuir binário:** construir em ambiente limpo da distribuição,
   conferir ABI/dependências e metadados. O pacote local registra o ambiente
   de construção em `.BUILDINFO`; sua existência não autoriza distribuí-lo.
   O pacote anterior à nova interface não integra o pacote de fontes atual.
   Sua construção local usou `--nodeps` para não instalar dependências: a
   compatibilidade exata das versões declaradas não foi certificada. Houve
   aviso interno de fakeroot; os proprietários/permissões do tar foram
   conferidos como root:root, 0755/0644. Namcap não apontou erros; avisou
   sobre o loader ELF e dependências invocadas por subprocessos.
   A receita original é um builder do checkout; a receita portátil gerada
   usa fonte identificada e SHA-256 conferido.
3. **Antes da aprovação do código:** executar a nova CI. O teste local não
   comprova seu funcionamento no runner Ubuntu 24.04.
4. **Interface:** a conexão e leitura inicial ainda ocorrem no startup.
   Snapshots periódicos usam várias leituras D-Bus; não constituem uma leitura
   atômica única. Perfis criados externamente exigem reabrir a lista. Acrescentar
   regressões de acessibilidade, falha durante fechamento e interação prolongada
   em hardware alvo; a captura isolada não cobre todas essas situações.
5. **DSP:** limiter de pico de amostra, sem true peak/oversampling/lookahead;
   aliases e artefatos de mudanças abruptas de parâmetros precisam de medição
   espectral e escuta. Estudar suavização de parâmetros antes de alterações
   que afetem o timbre. Crossfeed não é renderização binaural multicanal.
6. **Cobertura adicional:** definir/testar MSRV; acrescentar sanitizadores,
   fuzzing da ABI/JSON e ensaios de longa duração com hardware. O contrato
   LADSPA continua exigindo ponteiros/buffers válidos fornecidos pelo host.
7. **Perfis:** considerar pré-compilar regex e explicitar a prioridade por
   dispositivo. Hoje o casamento usa a ordem alfabética dos nomes dos perfis.

A varredura de publicação é heurística. Não houve reescrita de histórico,
acesso a memórias do Claude, pastas de clientes ou processos judiciais.
O wallpaper serviu como referência visual; seus pixels não foram incorporados
no ícone SVG. A revisão final de contexto sigiloso continua sendo humana.
