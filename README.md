# myClipboardPlayList

Monta uma fila de vídeos a partir do **histórico da área de transferência** e toca tudo no
**[mpv](https://mpv.io)**. Funciona com YouTube e qualquer outro site suportado pelo
**[yt-dlp](https://github.com/yt-dlp/yt-dlp)**.

Copie links de vídeos no navegador: eles entram na fila do app sozinhos, com thumbnail,
título e duração. Escolha um vídeo e aperte **Enter**.

## Funcionalidades

- Fila montada a partir do histórico do clipboard (hoje via `dms`), só com links http/https,
  sem repetições e na ordem em que foram copiados. A fila acompanha o histórico: o que é
  copiado entra sozinho, e o que sai da fila sai também do histórico.
- Thumbnail, título e duração de cada vídeo (obtidos com o yt-dlp), guardados em cache no
  disco para abrir o app sem consultar tudo de novo.
- Filtro por título ou site, com navegação completa pelo teclado.
- O mpv toca numa janela própria e **continua tocando ao fechar o app**. Ao reabrir, o app
  se reconecta a ele.
- Uma única instância: abrir o app de novo traz a janela existente para frente.
- Cookies do navegador (brave, brave-origin, chromium ou firefox) para os vídeos que o
  YouTube só libera com login, escolhidos no seletor **Cookies** do cabeçalho.
- Temas de cores (Meia-noite, Catppuccin Mocha, Dracula, Tokyo Night, Gruvbox e Claro),
  trocados pelo seletor ou com Ctrl+T. O tema e a visibilidade da ajuda são lembrados.

## Requisitos

| Requisito | Para quê | Como verificar |
|---|---|---|
| Linux | Sockets Unix e `XDG_RUNTIME_DIR` | — |
| [Rust](https://rustup.rs) **1.95 ou mais novo** | Compilar (exigido pelo `eframe` 0.36) | `rustc --version` |
| [mpv](https://mpv.io) | Reproduzir os vídeos | `mpv --version` |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Usado pelo mpv para abrir os links e pelo app para buscar título, duração e thumbnail | `yt-dlp --version` |
| [DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell) (`dms`) com o gerenciador de clipboard ativo | Ler, acompanhar e editar o histórico da área de transferência | `dms clipboard history --json` |
| Sessão gráfica Wayland ou X11 com OpenGL/Vulkan | Janela do app | — |

As bibliotecas gráficas (Wayland/X11, `libxkbcommon`, drivers de vídeo) são carregadas
quando o programa roda e já vêm em qualquer desktop Linux comum. Não é preciso instalar
pacotes de desenvolvimento nem OpenSSL (o download das thumbnails usa `rustls`).

### Instalando as dependências

Arch Linux:

```sh
sudo pacman -S rustup mpv yt-dlp
rustup default stable
```

Debian/Ubuntu (o `yt-dlp` dos repositórios costuma estar desatualizado; prefira a versão
oficial do projeto):

```sh
sudo apt install mpv curl
curl https://sh.rustup.rs -sSf | sh
# yt-dlp: veja https://github.com/yt-dlp/yt-dlp#installation
```

Para o `dms`, siga as instruções do
[DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell). O comando
`dms clipboard history --json` precisa listar o seu histórico. Ele depende do servidor do
DMS estar em execução.

> **Mantenha o yt-dlp atualizado.** Os sites mudam com frequência; quando vídeos param de
> tocar ou de mostrar título e thumbnail, quase sempre a solução é atualizar o yt-dlp.

## Compilando e executando

```sh
git clone https://github.com/gaugusto/myClipboardPlaylist.git myClipboardPlayList
cd myClipboardPlayList
cargo build --release
./target/release/my_clipboard_playlist
```

Ou, durante o desenvolvimento:

```sh
cargo run --release
```

### Instalando

```sh
cargo install --path .
```

O binário `my_clipboard_playlist` vai para `~/.cargo/bin` (que precisa estar no `PATH`).

Para o app aparecer no menu/lançador, crie `~/.local/share/applications/myclipboardplaylist.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=myClipboardPlayList
Comment=Fila de vídeos a partir da área de transferência
Exec=my_clipboard_playlist
Icon=multimedia-video-player
Terminal=false
Categories=AudioVideo;Video;Player;
StartupWMClass=myclipboardplaylist
```

O `app_id` da janela (Wayland) é `myclipboardplaylist`. Use esse nome em regras de janela do
compositor, como o `window-rule` do niri ou o `windowrule` do Hyprland.

## Como usar

1. Copie links de vídeos (YouTube, Vimeo, SoundCloud, …).
2. A fila acompanha o histórico do clipboard: o app o lê ao abrir, a cada cópia (via
   `dms clipboard watch`) e quando a janela volta ao foco. A fila passa a ter os links
   válidos do histórico: os novos entram no fim e os que saíram do histórico saem da fila.
   Se o histórico não tiver nenhum link válido, a fila fica como está.

   Links que não são vídeo ficam de fora. Arquivos de imagem ou documento (`.jpg`, `.pdf`,
   …) e fotos de posts do X (`/photo/…`) nem entram na fila. Os demais entram e, se o
   yt-dlp disser que não há vídeo ali ("Unsupported URL", "No video", "… is unavailable"),
   saem sozinhos (exceto o que estiver tocando). Esses links ficam anotados no cache e não
   voltam ao reabrir o app; o **Limpar** apaga essa lista. Falhas passageiras (sem
   rede, por exemplo) não removem nada.
3. Escolha um vídeo e aperte **Enter**, dê um clique duplo ou use o botão **▶**. Os
   próximos tocam em sequência. No vídeo que está tocando, o botão vira **⏸** e pausa;
   pausado, ele continua de onde parou (Enter e clique duplo fazem o mesmo). Tudo acontece
   no mesmo mpv: nenhum player novo é aberto.
4. Use o mpv normalmente para pausar (espaço), avançar ou voltar na fila (`>` / `<`) e
   fechar (`q`). Pausar pelo mpv também atualiza o botão no app.
5. Para mudar a ordem, **arraste o item** com o mouse: uma linha mostra onde ele vai
   cair, e a lista rola sozinha perto das bordas. A ordem é mantida quando o histórico
   muda e lembrada ao reabrir o app, mesmo que o mpv tenha fechado. Com o filtro ativo,
   o item cai logo antes do item visível que fica abaixo da linha. O vídeo que está
   tocando pode ser movido sem parar.
6. O **✕** de um item o tira da fila e também **exclui o link do histórico do clipboard**
   (`dms clipboard delete`, todas as cópias dele) e do cache, para que ele não volte.
7. O botão **Limpar** (ou **Ctrl+L**) pede confirmação: clique de novo (ou aperte Ctrl+L de
   novo) em até 4 segundos; Esc cancela. Ele para o mpv e esvazia a fila, **apaga todo o
   histórico do clipboard** (`dms clipboard clear`, não só os links) e limpa o cache de
   thumbnails, títulos e durações.

### Cookies do navegador

Alguns vídeos do YouTube só abrem com login ("Sign in to confirm you're not a bot"): ficam
"sem informações" e não tocam. No seletor **Cookies** do cabeçalho, escolha o navegador
em que você está logado no YouTube (brave, brave-origin, chromium ou firefox). O app passa
`--cookies-from-browser` ao yt-dlp, tanto nas consultas de título/thumbnail quanto no mpv
(via `ytdl-raw-options`, sem mexer nas outras opções do seu `mpv.conf`), e refaz as
consultas que tinham falhado. A escolha é lembrada; o padrão é **nenhum**.

- **brave-origin**: o yt-dlp não o conhece pelo nome; o app usa o perfil do Brave em
  `~/.config/BraveSoftware/Brave-Origin`.
- Nos navegadores baseados no Chromium, fora do KDE, o app força o chaveiro
  `gnomekeyring`: a detecção automática do yt-dlp falha em ambientes como niri e Hyprland
  e os cookies não são decifrados.

Itens novos entram na fila **sem tocar**: nada começa a tocar sozinho. Se o vídeo que está
tocando continuar no histórico, as mudanças na fila não o interrompem.

### Atalhos

O campo de filtro fica sempre em foco: é só digitar para filtrar.

| Tecla | Ação |
|---|---|
| ↑ / ↓ ou Ctrl+K / Ctrl+J | Percorrer a lista |
| Enter | Tocar o vídeo selecionado; no que está tocando, pausar/continuar |
| Ctrl+L | Limpar a fila, o histórico do clipboard e o cache (repita para confirmar) |
| Esc | Cancelar o Limpar; limpar o filtro; com o filtro vazio, fechar a janela |
| Ctrl+T | Trocar para o próximo tema |
| Ctrl+H | Mostrar/ocultar a ajuda de atalhos no rodapé |

## Como o app e o mpv convivem

- O app inicia **um único mpv** (`--idle`) e o controla pelo
  [JSON IPC](https://mpv.io/manual/master/#json-ipc). A janela do mpv só aparece quando um
  vídeo começa a tocar.
- **Ao fechar o app com algo tocando**, o mpv continua e fecha sozinho quando a fila
  terminar. **Com o mpv parado**, ele é encerrado junto com o app.
- **Ao reabrir o app**, ele se reconecta ao mpv que ficou tocando e mostra a fila dele.
- O mpv roda num grupo de processos próprio: um Ctrl+C no terminal que abriu o app não
  o derruba.

## Arquivos usados

| Caminho | Conteúdo |
|---|---|
| `$XDG_RUNTIME_DIR/myclipboardplaylist.sock` | Trava de instância única (removido ao fechar) |
| `$XDG_RUNTIME_DIR/myclipboardplaylist-mpv.sock` | Socket IPC do mpv |
| `~/.local/share/myclipboardplaylist/app.ron` | Tema escolhido, navegador dos cookies, ordem da fila, visibilidade da ajuda e tamanho da janela |
| `~/.cache/myclipboardplaylist/` (ou `$XDG_CACHE_HOME/myclipboardplaylist/`) | Cache de títulos, durações (`metadata.json`), thumbnails (`thumbs/`) e links sem vídeo (`nao_videos.json`); limpo pelo Limpar |

Se o app for encerrado à força, os sockets que ficarem para trás são detectados e
reaproveitados na próxima execução.

## Testes

```sh
cargo test                 # testes rápidos, sem rede
cargo test -- --ignored    # consulta real ao yt-dlp (precisa de internet)
cargo clippy --all-targets
```

## Solução de problemas

| Sintoma | Causa provável / solução |
|---|---|
| Status "não foi possível executar `dms`" ou "`dms … ` falhou" | `dms` não instalado, ou o servidor do DankMaterialShell não está em execução. Teste `dms clipboard history --json`. |
| Um link copiado não entra na fila | Só entram entradas que sejam **apenas** um link http/https; textos com um link no meio são ignorados. Se o `dms clipboard watch` não estiver funcionando, troque de janela e volte: o app relê o histórico ao ganhar foco. |
| "sem informações" e, no erro do yt-dlp (passe o mouse sobre o item), "Sign in to confirm you're not a bot" | O YouTube pede login para esse vídeo. Escolha no seletor **Cookies** o navegador em que você está logado no YouTube. Se aparecer "could not find … cookies database" ou "cannot decrypt", o navegador escolhido não é o que você usa (ou o chaveiro não está desbloqueado). |
| Itens com "sem informações" ou vídeo que não toca ("Falha ao reproduzir") | Atualize o yt-dlp. Passe o mouse sobre o item para ver a mensagem de erro do yt-dlp. |
| Um link de vídeo some da fila logo depois de entrar | O yt-dlp disse que não há vídeo nele (site sem suporte ou vídeo indisponível). Atualize o yt-dlp e use **Limpar** (Ctrl+L), que esquece os links rejeitados (e apaga o histórico). |
| Título ou thumbnail desatualizados | Vêm do cache. Remova o item (✕) e copie o link de novo para consultá-lo outra vez. |
| "não foi possível iniciar o mpv" | Instale o mpv e verifique se ele está no `PATH`. |
| Abrir o app não abre outra janela | É esperado (instância única). A janela existente é trazida para frente; alguns compositores Wayland apenas a marcam como pedindo atenção. |
| Erro de compilação citando `rust-version` | Atualize o Rust: `rustup update` (mínimo 1.95). |

## Estrutura do código

```
src/
  main.rs             inicialização, janela e instância única
  app.rs              interface (egui): cabeçalho, filtro, lista, rodapé e atalhos
  mpv.rs              inicia, reconecta e controla o mpv via JSON IPC
  history.rs          fontes de histórico do clipboard (trait HistorySource + dms)
  metadata.rs         título, duração e thumbnail via yt-dlp (4 consultas em paralelo)
  cache.rs            cache em disco dos metadados e das thumbnails
  cookies.rs          navegadores de onde o yt-dlp lê os cookies
  theme.rs            temas de cores (Palette) e estilo da interface
  single_instance.rs  trava de instância única
  url.rs              validação de links
```

### Estendendo

- **Outro gerenciador de clipboard** (ex.: cliphist): implemente `HistorySource` em
  `src/history.rs`: devolver os itens de texto do histórico (id e texto), do mais antigo
  para o mais recente, excluir um item, apagar tudo e abrir um processo que escreva uma
  linha a cada mudança no clipboard. O filtro de links e a remoção de repetidos já são
  feitos por `fetch_links`, e a exclusão de todas as cópias de um link, por `delete_link`.
- **Novo tema**: acrescente um `Theme` com sua `Palette` em `Theme::builtin()` em
  `src/theme.rs`. A interface usa apenas as cores da paleta.
