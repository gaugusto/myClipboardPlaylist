# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Visão geral

App desktop em Rust (GTK4/libadwaita, Linux) que monta uma fila de vídeos a partir do histórico
do clipboard (`dms clipboard history --json`) e a toca num único processo **mpv** controlado
por JSON IPC. Título, duração e thumbnail vêm do **yt-dlp**. Tudo no projeto (código,
comentários, mensagens da interface, README e commits) é escrito em **português**.

## Comandos

```sh
cargo build --release                 # binário em target/release/my_clipboard_playlist
cargo run --release
cargo test                            # testes rápidos, sem rede
cargo test -- --ignored               # consulta real ao yt-dlp (precisa de internet)
cargo test cache::tests::persists_and_clears   # um teste específico
cargo clippy --all-targets            # deve ficar sem avisos
cargo fmt
```

Exige Rust 1.92+ (edition 2024, `gtk4` 0.11) e GTK 4.18+ / libadwaita 1.7+ com os arquivos
de desenvolvimento. Em tempo de execução precisa de `mpv`, `yt-dlp` e `dms` no `PATH`.

## Arquitetura

**Fluxo de mensagens.** `App` (`src/app.rs`) é a única dona do estado e não conhece
widgets. Todo trabalho em segundo plano roda em threads que mandam um `AppMsg` (definido em
`src/main.rs`) por um `Notifier`; os módulos de backend recebem só o `Notifier` e não
dependem do GTK. Em `src/ui.rs`, o `Notifier` envia por um `async_channel`, que uma tarefa
do loop do GLib (`glib::spawn_future_local`) drena e entrega a `App::handle_message`. Para
uma fonte nova de eventos, acrescente uma variante em `AppMsg` em vez de compartilhar
estado com a interface.

**Interface.** `ui::Shared` guarda o `App` (num `RefCell`) e os widgets, montados uma vez.
Toda mudança passa por `Shared::update`, que aplica a ação ao `App` e chama
`Shared::render`, que acerta os widgets com o estado (as linhas da lista ficam em
`Shared::rows`, por `Entry::id`). Sinais de widgets usam `Shared::react`, que ignora os
disparados pelo próprio `render` (`syncing`). Prazos do `App` (confirmação do Limpar,
bloqueio de abertura) são vencidos por `App::expire`, agendado por `schedule_expiry`. As
configurações (`src/settings.rs`, `settings.json`) são gravadas quando mudam; o tamanho da
janela, só ao fechar. Na primeira execução, `Settings::load` migra o `app.ron` do eframe.

**mpv é a fonte da verdade da fila.** `App::playlist` é só um espelho: a interface manda
comandos (`append`, `play_index`, `remove`, `move_entry` em `src/mpv.rs`) e a playlist volta por
`observe_property` como `MpvEvent::Playlist`. Não altere `self.playlist` direto para
refletir uma ação; espere o evento. O mpv usa um socket de nome fixo em
`$XDG_RUNTIME_DIR` e roda num grupo de processos próprio, para **sobreviver ao app**. Ao
abrir, `Mpv::attach` reconecta a um mpv que ficou tocando. Se o mpv fecha com o app
aberto (`MpvEvent::Exited`), `App::restore_queue` mantém o espelho e refaz a fila num mpv
novo com `Mpv::restore` (um `loadlist` só); enquanto `App::restoring`, a playlist vazia do
mpv novo é ignorada. Ao fechar, `Mpv::detach` só encerra o mpv se ele estiver ocioso.

**Metadados.** Os itens são identificados pela URL (`Entry::filename`). `App::meta` guarda
um `MetaState` (`Loading` / `Ready` / `Failed`) por URL. `request_metadata` consulta primeiro
o `Cache` (`src/cache.rs`, em disco em `$XDG_CACHE_HOME/myclipboardplaylist`, com
`metadata.json` e `thumbs/`). Se não achar, enfileira no `Fetcher` (`src/metadata.rs`): 4
threads que rodam o yt-dlp, baixam a thumbnail com `ehttp`, gravam no cache e respondem com
`AppMsg::Metadata`. Falhas não vão para o cache, exceto as que `metadata::is_not_video`
reconhece (o link não tem vídeo): essas vão para a lista de rejeitados do `Cache`
(`nao_videos.json`), e `App::remove_not_video` tira o item da fila do mpv. A thumbnail
vem da cópia local ou, se o download falhou, é baixada da URL remota
(`request_remote_thumbs`, `AppMsg::Thumbnail`); `App::thumb` diz a origem e `ui::decode`
a decodifica (GTK para PNG/JPEG, crate `image` para WebP).

**Cookies.** `Browser` (`src/cookies.rs`) gera o valor de `--cookies-from-browser`. A escolha
(`App::set_browser`, salva pelo nome em `cookies_browser`) vai para o `Fetcher`
(`set_browser`) e para o mpv (`Mpv::set_cookies`, que altera só a chave
`cookies-from-browser` de `ytdl-raw-options` com `change-list` e a reaplica em `start`).

**Sincronia com o histórico.** Não há comando de atualizar: `App::refresh` relê o histórico
ao abrir, a cada `AppMsg::ClipboardChanged` (o `history::Watcher` roda `dms clipboard watch`)
e quando a janela ganha foco. `apply_history` acerta a fila de forma incremental
(`sync_plan`): o que saiu do histórico é removido e o que é novo entra no fim, sem mexer na
ordem, que o usuário reorganiza arrastando (`DragSource` em cada linha, `DropTarget` no
`ScrolledWindow`, em `src/ui.rs`) ou com Alt+↑/↓ (`App::move_selected`). Os dois chegam a
`App::drop_at`, que calcula o `playlist-move` com `move_plan` (posição entre os itens
visíveis) e chama `Mpv::move_entry`; até o mpv confirmar (`App::moving`), outra mudança de
posição é ignorada. A ordem é
salva em `queue_order` (`App::order`) e usada quando a fila é montada do zero. O
`sync_plan` também tira cópias repetidas de um link. Pedidos ao mpv ainda não refletidos
no espelho ficam em `App::removing` (por `Entry::id`, pois a URL pode se repetir) e
`App::adding` (por URL); índices enviados ao mpv vêm de `pending_index`. O ✕
(`remove_entry`) tira o item do mpv, do cache (`Cache::remove`) e do histórico
(`history::delete_link`). O Limpar (botão / Ctrl+L, `clear_command`, com segundo clique de
confirmação) para o mpv, limpa o cache e roda `dms clipboard clear`. Alterações no histórico
passam por `edit_history`, que incrementa `App::generation`: leituras iniciadas antes são
descartadas e o histórico é relido ao fim (`AppMsg::HistoryEdited`).

**Histórico do clipboard.** `HistorySource` (`src/history.rs`) abstrai o gerenciador de
clipboard. A implementação devolve os itens de texto (`Clip`, com o id), do mais antigo para o mais
recente, e sabe excluir um item, apagar tudo e acompanhar mudanças (`watch`).
`fetch_links` filtra os links válidos (`src/url.rs`, só textos que são apenas um link
http/https, sem imagens/documentos nem fotos do X) e remove repetidos.

**Teclado.** O campo de filtro recebe o que for digitado em qualquer lugar da janela
(`set_key_capture_widget`). Os atalhos ficam num `EventControllerKey` na fase de captura
da janela (`Shared::key_pressed`), antes do campo; setas, Enter e Esc só valem com o foco
no filtro ou na lista (`owns_keys`). Para um atalho novo: trate em `key_pressed` e
acrescente a dica em `ui::footer` e na tabela de atalhos do README.

**Cores.** Não há temas próprios: as cores vêm do tema GTK do sistema, incluindo o
`~/.config/gtk-4.0/gtk.css` (no ambiente do usuário, gerado pelo DMS). O `CSS` de
`src/ui.rs` só usa as cores nomeadas da libadwaita (`@accent_bg_color`, `@card_bg_color`,
`@error_bg_color`…), nunca cores fixas (exceto o selo de duração sobre a thumbnail).

**Instância única.** `src/single_instance.rs` usa um socket Unix em `$XDG_RUNTIME_DIR`. Uma
segunda execução avisa a primeira, que recebe `AppMsg::Activate` e traz a janela para
frente. Por isso o `adw::Application` não tem `application_id` (que ligaria a unicidade
por D-Bus): o `app-id` da janela no Wayland vem de `glib::set_prgname` e continua
`myclipboardplaylist`, usado em regras de janela do compositor.

## Documentação

O README descreve o comportamento visível (atalhos, arquivos usados, solução de problemas,
estrutura do código). Atualize-o junto quando mudar algo nessas áreas.
