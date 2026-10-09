# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Visão geral

App desktop em Rust (egui/eframe, Linux) que monta uma fila de vídeos a partir do histórico
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

Exige Rust 1.95+ (edition 2024, `eframe` 0.36). Em tempo de execução precisa de `mpv`,
`yt-dlp` e `dms` no `PATH`.

## Arquitetura

**Fluxo de mensagens.** `App` (`src/app.rs`) é a única dona do estado da interface. Todo
trabalho em segundo plano roda em threads que mandam um `AppMsg` (definido em
`src/main.rs`) por um `mpsc::Sender` e chamam `ctx.request_repaint()`. A interface drena o
canal em `App::handle_messages`, chamado em `eframe::App::logic`. Para uma fonte nova de
eventos, acrescente uma variante em `AppMsg` em vez de compartilhar estado com a interface.

**mpv é a fonte da verdade da fila.** `App::playlist` é só um espelho: a interface manda
comandos (`replace`, `play_index`, `remove` em `src/mpv.rs`) e a playlist volta por
`observe_property` como `MpvEvent::Playlist`. Não altere `self.playlist` direto para
refletir uma ação; espere o evento. O mpv usa um socket de nome fixo em
`$XDG_RUNTIME_DIR` e roda num grupo de processos próprio, para **sobreviver ao app**. Ao
abrir, `Mpv::attach` reconecta a um mpv que ficou tocando. Ao fechar, `Mpv::detach` só
encerra o mpv se ele estiver ocioso. `replace` não interrompe o item atual se ele continuar
na nova lista.

**Metadados.** Os itens são identificados pela URL (`Entry::filename`). `App::meta` guarda
um `MetaState` (`Loading` / `Ready` / `Failed`) por URL. `request_metadata` consulta primeiro
o `Cache` (`src/cache.rs`, em disco em `$XDG_CACHE_HOME/myclipboardplaylist`, com
`metadata.json` e `thumbs/`). Se não achar, enfileira no `Fetcher` (`src/metadata.rs`): 4
threads que rodam o yt-dlp, baixam a thumbnail com `ehttp`, gravam no cache e respondem com
`AppMsg::Metadata`. Falhas não vão para o cache, exceto as que `metadata::is_not_video`
reconhece (o link não tem vídeo): essas vão para a lista de rejeitados do `Cache`
(`nao_videos.json`), e `App::remove_not_video` tira o item da fila do mpv. A thumbnail é exibida de `file://` (cópia
local) ou, se o download falhou, da URL remota (loaders `file`/`http` do `egui_extras`).

**Atualizar.** O comando Atualizar (botão / Ctrl+R, `refresh_command`) limpa o cache (disco,
`App::meta` e `ctx.forget_all_images()`) e relê o histórico. A leitura automática ao abrir
(`refresh`) usa o cache e não mostra o aviso de "fila mantida" quando não há links.

**Histórico do clipboard.** `HistorySource` (`src/history.rs`) abstrai o gerenciador de
clipboard. A implementação só devolve os textos, do mais antigo para o mais recente.
`fetch_links` filtra os links válidos (`src/url.rs`, só textos que são apenas um link
http/https, sem imagens/documentos nem fotos do X) e remove repetidos.

**Teclado.** O campo de filtro fica sempre em foco. Por isso os atalhos são consumidos com
`consume_key` em `App::handle_keys` **antes** de o campo ser desenhado, e o campo usa um
`EventFilter` para ficar com as setas e o Esc. Para um atalho novo: consuma em
`handle_keys` e acrescente a dica em `App::footer` e na tabela de atalhos do README.

**Temas.** A interface usa só os papéis de `Palette` (`src/theme.rs`). Um tema novo é uma
entrada em `Theme::builtin()`. A ordem dessa lista é a ordem do Ctrl+T. O tema é salvo
pelo **nome** no armazenamento do eframe. Ao renomear um tema, mapeie o nome antigo em
`Theme::migrate_name`.

**Instância única.** `src/single_instance.rs` usa um socket Unix em `$XDG_RUNTIME_DIR`. Uma
segunda execução avisa a primeira, que recebe `AppMsg::Activate` e traz a janela para
frente.

## Documentação

O README descreve o comportamento visível (atalhos, arquivos usados, solução de problemas,
estrutura do código). Atualize-o junto quando mudar algo nessas áreas.
