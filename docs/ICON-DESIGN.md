# BigSound — alto-falante e assinatura Big

Revisão de 11/10/2026. A direção aprovada usa uma base quadrada arredondada
em grafite, um alto-falante frontal simplificado e um único acento ciano.
A assinatura Big aparece acima do alto-falante. A composição privilegia a
leitura de áudio em tamanho pequeno e a coesão com os Big Apps.

As proporções e a simplicidade tomam como referência os ícones instalados
de Ashyterm e Ashypass. A coruja desses aplicativos não faz parte deste desenho.
A marca Big reutiliza o traçado de `path72` do ícone `big-audio-converter.svg`
do BigLinux, distribuído pelo pacote `big-audio-converter` em
`/usr/share/icons/hicolor/scalable/apps/`. A marca é preexistente;
a composição do BigSound é nova. Nenhum wallpaper ou bitmap é incorporado.

## Arquivos

- `crates/gtk-app/data/icons/com.bigcommunity.BigSound.svg`: ícone colorido,
  viewBox 128×128, formas sólidas e profundidade inferior discreta.
- `crates/gtk-app/data/icons/com.bigcommunity.BigSound-symbolic.svg`: versão
  monocromática 16×16, com tweeter e cone. A assinatura pequena é omitida
  nesta versão para preservar a leitura e a aplicação das cores do tema.
- `assets/bigsound-speaker-big.png`: exportação transparente 512×512 para o README.
- `assets/icon.png`: a mesma exportação, para compatibilidade com o caminho anterior.

Os SVGs são autocontidos, sem bitmap, scripts, filtros ou recursos externos.
A exportação PNG não contém EXIF nem campos textuais. As capturas são feitas
com o aplicativo real em uma sessão privada de GTK/X11, D-Bus e PipeWire,
sem dispositivos físicos ou desktop pessoal. Os testes usam escala nativa
2× e os temas claro e escuro; a inspeção do ícone inclui 16, 32, 48, 64 e 128 px.
