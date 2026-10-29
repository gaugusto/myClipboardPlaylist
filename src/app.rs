//! Estado e regras da fila, sem nada da interface: `ui` desenha a partir daqui e chama os
//! métodos de `App` em resposta ao usuário e às mensagens das threads.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::cookies::Browser;
use crate::history::{self, HistorySource, Watcher};
use crate::metadata::{self, Fetcher, Metadata};
use crate::mpv::{Entry, Mpv, MpvEvent};
use crate::settings::Settings;
use crate::{AppMsg, Notifier};

/// Tempo sem resposta do mpv depois do qual um vídeo pedido deixa de bloquear os outros.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Prazo para confirmar o Limpar com um segundo clique.
const CONFIRM_CLEAR: Duration = Duration::from_secs(4);
/// Tempo máximo à espera de o mpv confirmar uma mudança de posição, antes de aceitar outra.
const MOVE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct Status {
    pub text: String,
    pub is_error: bool,
}

impl Status {
    fn info(text: impl Into<String>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            is_error: false,
        })
    }

    fn error(text: impl Into<String>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            is_error: true,
        })
    }
}

/// Como acertar a fila do mpv (`queue`, com o item atual em `current`) com os links do
/// histórico: as posições que saem (em ordem crescente) e os links que entram no fim.
/// Saem os links que não estão mais no histórico e as cópias repetidas (fica o item atual
/// ou, se nenhuma cópia for a atual, a primeira). A ordem da fila é mantida; numa fila
/// vazia, os links já conhecidos entram na ordem salva (`order`) e os outros depois, na
/// ordem do histórico.
fn sync_plan(
    queue: &[String],
    current: Option<usize>,
    mut links: Vec<String>,
    order: &[String],
) -> (Vec<usize>, Vec<String>) {
    if queue.is_empty() {
        let rank = |l: &String| order.iter().position(|o| o == l);
        links.sort_by_key(|l| rank(l).unwrap_or(usize::MAX));
    }
    let keep = |i: usize| {
        let url = &queue[i];
        match current.filter(|&c| queue[c] == *url) {
            Some(c) => i == c,
            None => queue.iter().position(|q| q == url) == Some(i),
        }
    };
    let gone = (0..queue.len())
        .filter(|&i| !links.contains(&queue[i]) || !keep(i))
        .collect();
    let new = links.into_iter().filter(|l| !queue.contains(l)).collect();
    (gone, new)
}

/// Como mover o item `id` para a posição `slot` entre os itens visíveis (`visible`, ids na
/// ordem da tela): os índices `from` e `to` do `playlist-move` do mpv, contados na fila
/// `pending`. O item fica antes do visível que ocupava a posição `slot` (com o filtro ativo,
/// logo antes dele) ou, se `slot` passar do último, logo depois do último visível. `None`
/// se o item ficaria onde está.
fn move_plan(pending: &[u64], visible: &[u64], id: u64, slot: usize) -> Option<(usize, usize)> {
    let index = |id: &u64| pending.iter().position(|p| p == id);
    let from = index(&id)?;
    let to = match visible.get(slot) {
        Some(before) => index(before)?,
        None => index(visible.last()?)? + 1,
    };
    (to != from && to != from + 1).then_some((from, to))
}

pub enum MetaState {
    Loading,
    Ready(Metadata),
    Failed(String),
}

/// Thumbnail baixada da URL remota, quando a cópia no cache não existe.
enum RemoteThumb {
    Loading,
    Ready(Arc<[u8]>),
    Failed,
}

/// De onde vem a imagem da thumbnail de um item.
pub enum Thumb<'a> {
    File(&'a Path),
    Bytes(&'a Arc<[u8]>),
    /// Ainda consultando o yt-dlp ou baixando a imagem.
    Loading,
    None,
}

pub struct App {
    mpv: Mpv,
    notifier: Notifier,
    source: Arc<dyn HistorySource>,
    loading: bool,
    /// Pediram outra leitura do histórico enquanto uma estava em andamento.
    refresh_again: bool,
    /// Muda a cada alteração no histórico do clipboard; leituras que começaram antes
    /// dela são descartadas.
    generation: u64,
    /// Alterações no histórico do clipboard ainda em andamento.
    editing: usize,
    /// Faz as alterações no histórico uma de cada vez.
    edit_lock: Arc<Mutex<()>>,
    /// Acompanha o clipboard e relê o histórico a cada mudança.
    _watcher: Watcher,
    /// Momento do primeiro clique no Limpar, à espera da confirmação.
    confirm_clear: Option<Instant>,
    pub playlist: Vec<Entry>,
    /// Ordem da fila (URLs), salva nas configurações.
    order: Vec<String>,
    title: Option<String>,
    /// Títulos informados pelo mpv ao tocar, por URL (reserva se o yt-dlp falhar).
    titles: HashMap<String, String>,
    fetcher: Fetcher,
    /// Cache em disco dos metadados; esvaziado pelo comando Limpar.
    cache: Arc<Cache>,
    /// Metadados do yt-dlp (título, duração, thumbnail), por URL.
    meta: HashMap<String, MetaState>,
    /// Thumbnails baixadas da URL remota, por URL do vídeo.
    remote_thumbs: HashMap<String, RemoteThumb>,
    pub status: Option<Status>,
    /// mpv ocioso (nada tocando); decide se ele continua aberto quando o app fecha.
    mpv_idle: bool,
    /// Links cuja remoção já foi pedida ao mpv, mas que ainda estão no espelho.
    removing: HashSet<u64>,
    /// Links cuja entrada na fila já foi pedida ao mpv, mas que ainda não estão no espelho.
    adding: HashSet<String>,
    /// O mpv foi fechado e a fila está sendo refeita num novo: até ela chegar, o espelho
    /// mantém a fila anterior (e ignora a playlist vazia do mpv novo).
    restoring: bool,
    /// Vídeo pedido ao mpv que ainda não começou a tocar (id e momento do pedido): até ele
    /// começar ou falhar, nenhum outro pode ser aberto.
    opening: Option<(u64, Instant)>,
    /// Momento da última mudança de posição pedida ao mpv e ainda não refletida no espelho:
    /// outra só é aceita depois, para não ser calculada sobre a ordem antiga.
    moving: Option<Instant>,
    /// Links sem vídeo removidos da fila desde a última leitura do histórico.
    not_videos: usize,
    /// Reprodução pausada no mpv (espelho da propriedade `pause`).
    paused: bool,
    pub filter: String,
    /// URL do item selecionado pelo teclado/clique (sobrevive a mudanças na fila).
    pub selected: Option<String>,
    /// Rola a lista até o item selecionado no próximo desenho.
    pub scroll_to_selected: bool,
    /// Rodapé com a ajuda de atalhos (Ctrl+H alterna).
    pub show_help: bool,
    /// Navegador de onde o yt-dlp (do app e do mpv) lê os cookies.
    pub browser: Option<Browser>,
}

impl App {
    pub fn new(notifier: Notifier, settings: &Settings) -> Self {
        let cache = Arc::new(Cache::load());
        let source: Arc<dyn HistorySource> = Arc::new(history::Dms);
        let watcher = {
            let notifier = notifier.clone();
            Watcher::start(source.clone(), move || {
                notifier.send(AppMsg::ClipboardChanged);
            })
        };
        let mut app = Self {
            fetcher: Fetcher::new(notifier.clone(), cache.clone()),
            cache,
            meta: HashMap::new(),
            remote_thumbs: HashMap::new(),
            mpv: Mpv::new(notifier.clone()),
            notifier,
            source,
            loading: false,
            refresh_again: false,
            generation: 0,
            editing: 0,
            edit_lock: Arc::new(Mutex::new(())),
            _watcher: watcher,
            confirm_clear: None,
            playlist: Vec::new(),
            order: settings.queue_order.clone(),
            title: None,
            titles: HashMap::new(),
            status: None,
            mpv_idle: true,
            removing: HashSet::new(),
            adding: HashSet::new(),
            restoring: false,
            opening: None,
            moving: None,
            not_videos: 0,
            paused: false,
            filter: String::new(),
            selected: None,
            scroll_to_selected: false,
            show_help: settings.show_help,
            browser: None,
        };
        app.set_browser(Browser::from_name(&settings.cookies_browser));
        if let Some(playlist) = app.mpv.attach() {
            app.playlist = playlist;
            app.request_metadata();
            app.status = Status::info("Reconectado ao mpv que continuou tocando.");
        }
        app.refresh();
        app
    }

    /// As configurações a salvar (o tamanho da janela fica por conta da interface).
    pub fn settings(&self, window_size: Option<(i32, i32)>) -> Settings {
        Settings {
            show_help: self.show_help,
            queue_order: self.order.clone(),
            cookies_browser: self.browser.map_or("", Browser::name).to_owned(),
            window_size,
        }
    }

    /// Ao fechar a janela: o mpv só é encerrado se estiver ocioso.
    pub fn shutdown(&mut self) {
        self.mpv.detach(self.mpv_idle);
    }

    pub fn source_name(&self) -> &str {
        self.source.name()
    }

    fn report(&mut self, result: anyhow::Result<()>) {
        if let Err(e) = result {
            self.status = Status::error(format!("{e:#}"));
        }
    }

    /// Lê o histórico do clipboard em segundo plano.
    pub fn refresh(&mut self) {
        if self.loading {
            self.refresh_again = true;
            return;
        }
        self.loading = true;
        let (notifier, source) = (self.notifier.clone(), self.source.clone());
        let generation = self.generation;
        thread::spawn(move || {
            let result = history::fetch_links(source.as_ref()).map_err(|e| format!("{e:#}"));
            notifier.send(AppMsg::History(generation, result));
        });
    }

    /// Altera o histórico do clipboard em segundo plano; `edit` devolve o aviso a mostrar.
    /// Leituras que começaram antes são descartadas, e o histórico é relido no fim.
    fn edit_history(
        &mut self,
        edit: impl FnOnce(&dyn HistorySource) -> anyhow::Result<Option<String>> + Send + 'static,
    ) {
        self.generation += 1;
        self.editing += 1;
        let (notifier, source) = (self.notifier.clone(), self.source.clone());
        let lock = self.edit_lock.clone();
        thread::spawn(move || {
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let result = edit(source.as_ref()).map_err(|e| format!("{e:#}"));
            notifier.send(AppMsg::HistoryEdited(result));
        });
    }

    /// Comando Limpar (botão ou Ctrl+L, repetido para confirmar): esvazia a fila, o
    /// histórico do clipboard e o cache de metadados.
    pub fn clear_command(&mut self) {
        if !self.clear_armed() {
            self.confirm_clear = Some(Instant::now());
            return;
        }
        self.confirm_clear = None;
        let stopped = self.mpv.clear();
        self.report(stopped);
        let cleared = self.cache.clear();
        self.report(cleared);
        self.meta.clear();
        self.titles.clear();
        self.remote_thumbs.clear();
        self.edit_history(|source| {
            source.clear()?;
            Ok(Some(
                "Fila, histórico do clipboard e cache limpos.".to_owned(),
            ))
        });
    }

    /// O Limpar já foi clicado uma vez e espera a confirmação.
    pub fn clear_armed(&self) -> bool {
        self.confirm_clear
            .is_some_and(|t| t.elapsed() < CONFIRM_CLEAR)
    }

    /// Remove o item da fila e também do histórico do clipboard e do cache. As outras
    /// cópias do link, se houver, saem na próxima leitura do histórico.
    pub fn remove_entry(&mut self, id: u64) {
        let Some(url) = self
            .pending()
            .find(|e| e.id == id)
            .map(|e| e.filename.clone())
        else {
            return;
        };
        let removed = self.remove_id(id);
        if removed.is_err() {
            self.report(removed);
            return;
        }
        let url = url.as_str();
        self.meta.remove(url);
        self.remote_thumbs.remove(url);
        self.titles.remove(url);
        let uncached = self.cache.remove(url);
        self.report(uncached);
        let url = url.to_owned();
        self.edit_history(move |source| history::delete_link(source, &url).map(|_| None));
    }

    /// A fila como o mpv a vê: o espelho pode ainda conter itens cuja remoção já foi
    /// pedida, e eles não contam.
    fn pending(&self) -> impl Iterator<Item = &Entry> {
        self.playlist
            .iter()
            .filter(|e| !self.removing.contains(&e.id))
    }

    /// Posição do item `id` na fila como o mpv a vê.
    fn pending_index(&self, id: u64) -> Option<usize> {
        self.pending().position(|e| e.id == id)
    }

    /// Pede ao mpv para tirar o item `id` da fila.
    fn remove_id(&mut self, id: u64) -> anyhow::Result<()> {
        let Some(index) = self.pending_index(id) else {
            return Ok(());
        };
        self.mpv.remove(index)?;
        self.removing.insert(id);
        Ok(())
    }

    /// Acerta a fila com os links do clipboard, a menos que não haja nenhum: os que saíram
    /// do histórico saem da fila e os novos entram no fim. A ordem da fila (que o usuário
    /// pode ter reorganizado) é mantida.
    fn apply_history(&mut self, generation: u64, result: Result<Vec<String>, String>) {
        self.loading = false;
        // Se o histórico mudou (ou está mudando) desde que a leitura começou, ela está
        // velha. Quem o alterou pede outra leitura ao terminar.
        let stale = generation != self.generation || self.editing > 0;
        if std::mem::take(&mut self.refresh_again) && self.editing == 0 {
            self.refresh();
        }
        if stale {
            return;
        }
        let mut links = match result {
            Ok(links) => links,
            Err(e) => {
                self.status = Status::error(e);
                return;
            }
        };
        links.retain(|l| !self.cache.is_rejected(l));
        // Um clipboard sem links não esvazia a fila.
        if links.is_empty() {
            return;
        }
        let entries: Vec<(u64, String, bool)> = self
            .pending()
            .map(|e| (e.id, e.filename.clone(), e.current))
            .collect();
        let queue: Vec<String> = entries.iter().map(|(_, url, _)| url.clone()).collect();
        let current = entries.iter().position(|&(_, _, current)| current);
        // Links já pedidos ao mpv (e ainda fora do espelho) não são pedidos de novo.
        links.retain(|l| !self.adding.contains(l));
        let (gone, new) = sync_plan(&queue, current, links, &self.order);
        if gone.is_empty() && new.is_empty() {
            return;
        }
        self.not_videos = 0;
        // De trás para a frente, para os índices seguintes continuarem valendo.
        for &i in gone.iter().rev() {
            let removed = self.mpv.remove(i);
            if removed.is_err() {
                self.report(removed);
                return;
            }
            self.removing.insert(entries[i].0);
        }
        let result = self.mpv.append(&new);
        if result.is_ok() {
            self.adding.extend(new.iter().cloned());
        }
        self.status = if result.is_ok() {
            Status::info(format!(
                "Fila atualizada: {} link(s).",
                // `adding` já inclui os links de `new`.
                queue.len() - gone.len() + self.adding.len()
            ))
        } else {
            None
        };
        self.report(result);
    }

    fn remember_current_title(&mut self) {
        let (Some(title), Some(entry)) = (&self.title, self.playlist.iter().find(|e| e.current))
        else {
            return;
        };
        if *title != entry.filename {
            self.titles.insert(entry.filename.clone(), title.clone());
        }
    }

    /// Trata uma mensagem das threads de segundo plano. `AppMsg::Activate` é da interface.
    pub fn handle_message(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::History(generation, result) => self.apply_history(generation, result),
            AppMsg::ClipboardChanged => self.refresh(),
            AppMsg::HistoryEdited(result) => {
                self.editing -= 1;
                match result {
                    Ok(Some(text)) => self.status = Status::info(text),
                    Ok(None) => {}
                    Err(e) => self.status = Status::error(e),
                }
                self.refresh();
            }
            AppMsg::Activate => {}
            AppMsg::Metadata(url, result) => {
                let not_video = result.as_ref().is_err_and(|e| metadata::is_not_video(e));
                let state = result.map_or_else(MetaState::Failed, MetaState::Ready);
                self.meta.insert(url.clone(), state);
                if not_video {
                    self.remove_not_video(&url);
                }
                self.request_remote_thumbs();
            }
            AppMsg::Thumbnail(url, result) => {
                // Pode ter chegado depois de o item sair da fila ou do Limpar.
                if let Some(thumb) = self.remote_thumbs.get_mut(&url) {
                    *thumb = match result {
                        Ok(bytes) => RemoteThumb::Ready(bytes.into()),
                        Err(e) => {
                            eprintln!("aviso: {e}");
                            RemoteThumb::Failed
                        }
                    };
                }
            }
            AppMsg::Mpv(event) => self.handle_mpv(event),
        }
    }

    fn handle_mpv(&mut self, event: MpvEvent) {
        match event {
            // A playlist vazia do mpv novo chegaria antes da fila refeita.
            MpvEvent::Playlist(list) if self.restoring && list.is_empty() => {}
            MpvEvent::Playlist(list) => {
                self.restoring = false;
                self.moving = None;
                // O vídeo pedido saiu da fila: não vai mais tocar.
                if self
                    .opening
                    .is_some_and(|(id, _)| !list.iter().any(|e| e.id == id))
                {
                    self.opening = None;
                }
                // Remoções e entradas que o mpv já aplicou deixam de estar pendentes.
                self.removing.retain(|&id| list.iter().any(|e| e.id == id));
                self.adding
                    .retain(|url| !list.iter().any(|e| e.filename == *url));
                self.order = list.iter().map(|e| e.filename.clone()).collect();
                self.playlist = list;
                self.remember_current_title();
                self.request_metadata();
            }
            MpvEvent::Title(t) => {
                self.title = t;
                self.remember_current_title();
            }
            MpvEvent::Idle(idle) => {
                // Ocioso: o vídeo pedido não vai mais tocar (foi parado ou removido).
                if idle {
                    self.opening = None;
                }
                self.mpv_idle = idle;
            }
            MpvEvent::Pause(paused) => self.paused = paused,
            MpvEvent::FileError(e) => {
                self.opening = None;
                self.status = Status::error(e);
            }
            MpvEvent::PlaybackStarted => self.opening = None,
            MpvEvent::Exited => {
                self.opening = None;
                self.moving = None;
                self.restore_queue();
            }
        }
    }

    /// O mpv foi fechado (pelo `q`, por exemplo): a fila continua na tela e é refeita, na
    /// mesma ordem, num mpv novo e ocioso, que só abre janela ao tocar.
    fn restore_queue(&mut self) {
        self.mpv_idle = true;
        self.paused = false;
        self.title = None;
        let removing = std::mem::take(&mut self.removing);
        self.playlist.retain(|e| !removing.contains(&e.id));
        self.adding.clear();
        for entry in &mut self.playlist {
            entry.current = false;
        }
        if self.playlist.is_empty() {
            return;
        }
        let links: Vec<String> = self.playlist.iter().map(|e| e.filename.clone()).collect();
        let result = self.mpv.restore(&links);
        self.restoring = result.is_ok();
        if result.is_err() {
            self.playlist.clear();
        }
        self.report(result);
    }

    /// Busca no cache, ou pede ao yt-dlp, os metadados dos itens da fila que ainda não
    /// foram consultados.
    fn request_metadata(&mut self) {
        let rejected: Vec<String> = self
            .playlist
            .iter()
            .filter(|e| self.cache.is_rejected(&e.filename))
            .map(|e| e.filename.clone())
            .collect();
        for url in &rejected {
            self.remove_not_video(url);
        }
        for entry in &self.playlist {
            if self.meta.contains_key(&entry.filename)
                || rejected.contains(&entry.filename)
                || self.removing.contains(&entry.id)
            {
                continue;
            }
            let state = match self.cache.get(&entry.filename) {
                Some(meta) => MetaState::Ready(meta),
                None => {
                    self.fetcher.request(entry.filename.clone());
                    MetaState::Loading
                }
            };
            self.meta.insert(entry.filename.clone(), state);
        }
        self.request_remote_thumbs();
    }

    /// Baixa a thumbnail da URL remota dos itens cuja cópia no cache não existe (o download
    /// pelo `Fetcher` falhou ou o metadado é antigo).
    fn request_remote_thumbs(&mut self) {
        for entry in &self.playlist {
            let Some(MetaState::Ready(Metadata {
                thumb_file: None,
                thumbnail: Some(thumb_url),
                ..
            })) = self.meta.get(&entry.filename)
            else {
                continue;
            };
            if self.remote_thumbs.contains_key(&entry.filename) {
                continue;
            }
            self.remote_thumbs
                .insert(entry.filename.clone(), RemoteThumb::Loading);
            let (notifier, url) = (self.notifier.clone(), entry.filename.clone());
            ehttp::fetch(ehttp::Request::get(thumb_url), move |response| {
                let result = match response {
                    Ok(r) if r.ok => Ok(r.bytes),
                    Ok(r) => Err(format!("falha ao baixar a thumbnail: HTTP {}", r.status)),
                    Err(e) => Err(format!("falha ao baixar a thumbnail: {e}")),
                };
                notifier.send(AppMsg::Thumbnail(url, result));
            });
        }
    }

    /// Tira da fila um link em que o yt-dlp não achou vídeo. O item que está tocando
    /// fica: se o mpv conseguiu abri-lo, é mídia.
    fn remove_not_video(&mut self, url: &str) {
        if self.playlist.iter().any(|e| e.current && e.filename == url) {
            return;
        }
        let ids: Vec<u64> = self
            .pending()
            .filter(|e| e.filename == url)
            .map(|e| e.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let r = ids.into_iter().try_for_each(|id| self.remove_id(id));
        if r.is_ok() {
            self.not_videos += 1;
            self.status = Status::info(format!(
                "{} link(s) sem vídeo removido(s) da fila.",
                self.not_videos
            ));
        }
        self.report(r);
    }

    pub fn entry_title(&self, entry: &Entry) -> String {
        let from_meta = match self.meta.get(&entry.filename) {
            Some(MetaState::Ready(m)) => m.title.clone(),
            _ => None,
        };
        from_meta
            .or_else(|| entry.title.clone())
            .or_else(|| self.titles.get(&entry.filename).cloned())
            .unwrap_or_else(|| entry.filename.clone())
    }

    /// Linha de detalhes: domínio do link e, se for o caso, o estado da consulta.
    /// (A duração aparece no selo sobre a thumbnail.)
    pub fn entry_details(&self, entry: &Entry) -> String {
        let mut parts = Vec::new();
        if let Some(host) = url::Url::parse(&entry.filename)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
        {
            parts.push(host.trim_start_matches("www.").to_owned());
        }
        match self.meta.get(&entry.filename) {
            Some(MetaState::Loading) | None => parts.push("carregando informações…".to_owned()),
            Some(MetaState::Failed(_)) => parts.push("sem informações".to_owned()),
            Some(MetaState::Ready(_)) => {}
        }
        parts.join("  ·  ")
    }

    /// Erro do yt-dlp ao consultar o item, se houve.
    pub fn entry_error(&self, entry: &Entry) -> Option<&str> {
        match self.meta.get(&entry.filename) {
            Some(MetaState::Failed(e)) => Some(e),
            _ => None,
        }
    }

    pub fn duration_badge(&self, entry: &Entry) -> Option<String> {
        match self.meta.get(&entry.filename) {
            Some(MetaState::Ready(m)) if m.is_live => Some("AO VIVO".to_owned()),
            Some(MetaState::Ready(m)) => m.duration.map(metadata::format_duration),
            _ => None,
        }
    }

    /// Imagem da thumbnail de `url`: prefere a cópia local (cache) à baixada da URL remota.
    pub fn thumb(&self, url: &str) -> Thumb<'_> {
        match self.meta.get(url) {
            Some(MetaState::Ready(Metadata {
                thumb_file: Some(file),
                ..
            })) => Thumb::File(file),
            Some(MetaState::Ready(_)) => match self.remote_thumbs.get(url) {
                Some(RemoteThumb::Ready(bytes)) => Thumb::Bytes(bytes),
                Some(RemoteThumb::Loading) => Thumb::Loading,
                Some(RemoteThumb::Failed) | None => Thumb::None,
            },
            Some(MetaState::Loading) | None => Thumb::Loading,
            Some(MetaState::Failed(_)) => Thumb::None,
        }
    }

    /// O item é o atual do mpv e não está pausado.
    pub fn is_playing(&self, entry: &Entry) -> bool {
        entry.current && !self.mpv_idle && !self.paused
    }

    /// O item é o atual e o mpv não está ocioso (tocando ou pausado).
    pub fn is_active(&self, entry: &Entry) -> bool {
        entry.current && !self.mpv_idle
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Id do vídeo pedido ao mpv que ainda não começou a tocar.
    pub fn opening(&self) -> Option<u64> {
        self.opening.map(|(id, _)| id)
    }

    /// Libera o que expirou (a confirmação do Limpar e o bloqueio de abertura) e diz daqui
    /// a quanto tempo o próximo prazo vence.
    pub fn expire(&mut self) -> Option<Duration> {
        if self.confirm_clear.is_some() && !self.clear_armed() {
            self.confirm_clear = None;
        }
        if self
            .opening
            .is_some_and(|(_, since)| since.elapsed() >= OPEN_TIMEOUT)
        {
            self.opening = None;
        }
        let clear = self.confirm_clear.map(|t| CONFIRM_CLEAR - t.elapsed());
        let open = self.opening.map(|(_, t)| OPEN_TIMEOUT - t.elapsed());
        clear.into_iter().chain(open).min()
    }

    pub fn set_browser(&mut self, browser: Option<Browser>) {
        self.browser = browser;
        self.fetcher.set_browser(browser);
        let applied = self.mpv.set_cookies(browser.map(Browser::ytdlp_arg));
        self.report(applied);
    }

    /// Troca o navegador dos cookies escolhido no seletor.
    pub fn choose_browser(&mut self, browser: Option<Browser>) {
        if browser == self.browser {
            return;
        }
        self.set_browser(browser);
        // As consultas que falharam (talvez por falta de login) são refeitas.
        self.meta.retain(|_, m| !matches!(m, MetaState::Failed(_)));
        self.request_metadata();
    }

    pub fn set_filter(&mut self, filter: &str) {
        if filter == self.filter {
            return;
        }
        self.filter = filter.to_owned();
        self.selected = self
            .visible()
            .first()
            .map(|&i| self.playlist[i].filename.clone());
        self.scroll_to_selected = true;
    }

    /// Esc: cancela o Limpar e limpa o filtro. Devolve `false` se não havia nada a fazer
    /// (a janela deve fechar).
    pub fn escape(&mut self) -> bool {
        if self.clear_armed() {
            self.confirm_clear = None;
        } else if self.filter.is_empty() {
            return false;
        } else {
            self.filter.clear();
            self.scroll_to_selected = true;
        }
        true
    }

    /// Texto da contagem da fila (com o filtro, quantos aparecem).
    pub fn count_text(&self) -> String {
        let total = self.playlist.len();
        let count = match total {
            0 => "Fila vazia".to_owned(),
            1 => "1 vídeo na fila".to_owned(),
            n => format!("{n} vídeos na fila"),
        };
        if self.filter.trim().is_empty() || total == 0 {
            count
        } else {
            format!("{} de {count}", self.visible().len())
        }
    }

    /// Índices (na playlist) dos itens que passam pelo filtro.
    /// Cada palavra do filtro precisa aparecer no título ou na URL.
    pub fn visible(&self) -> Vec<usize> {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        (0..self.playlist.len())
            .filter(|&i| {
                let entry = &self.playlist[i];
                let haystack =
                    format!("{} {}", self.entry_title(entry), entry.filename).to_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .collect()
    }

    fn selected_index(&self, visible: &[usize]) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        visible
            .iter()
            .position(|&i| self.playlist[i].filename == selected)
    }

    /// Mantém sempre um item selecionado entre os visíveis.
    pub fn keep_selection(&mut self) {
        let visible = self.visible();
        if self.selected_index(&visible).is_none() {
            self.selected = visible.first().map(|&i| self.playlist[i].filename.clone());
        }
    }

    /// Move a seleção `delta` posições dentro da lista filtrada.
    pub fn move_selection(&mut self, delta: isize) {
        let visible = self.visible();
        let Some(last) = visible.len().checked_sub(1) else {
            return;
        };
        let pos = match self.selected_index(&visible) {
            Some(pos) => pos.saturating_add_signed(delta).min(last),
            None if delta > 0 => 0,
            None => last,
        };
        self.selected = Some(self.playlist[visible[pos]].filename.clone());
        self.scroll_to_selected = true;
    }

    pub fn select(&mut self, id: u64) {
        if let Some(entry) = self.playlist.iter().find(|e| e.id == id) {
            self.selected = Some(entry.filename.clone());
        }
    }

    /// Move o item `id` para a posição `slot` entre os itens visíveis (quantos visíveis
    /// ficam acima dele). Vale para o arrastar e para o Alt+↑/↓.
    pub fn drop_at(&mut self, id: u64, slot: usize) {
        if self.moving.is_some_and(|t| t.elapsed() < MOVE_TIMEOUT) {
            return;
        }
        let pending: Vec<u64> = self.pending().map(|e| e.id).collect();
        let visible: Vec<u64> = self
            .visible()
            .into_iter()
            .map(|i| self.playlist[i].id)
            .filter(|id| pending.contains(id))
            .collect();
        let Some((from, to)) = move_plan(&pending, &visible, id, slot) else {
            return;
        };
        let moved = self.mpv.move_entry(from, to);
        if moved.is_ok() {
            self.moving = Some(Instant::now());
        }
        self.report(moved);
    }

    /// Alt+↑/↓: move o item selecionado uma posição entre os visíveis.
    pub fn move_selected(&mut self, delta: isize) {
        let visible = self.visible();
        let Some(pos) = self.selected_index(&visible) else {
            return;
        };
        let slot = if delta > 0 {
            // Para depois do próximo visível, se houver.
            if pos + 1 >= visible.len() {
                return;
            }
            pos + 2
        } else {
            let Some(slot) = pos.checked_sub(1) else {
                return;
            };
            slot
        };
        self.drop_at(self.playlist[visible[pos]].id, slot);
        self.scroll_to_selected = true;
    }

    pub fn play_selected(&mut self) {
        let visible = self.visible();
        if let Some(pos) = self.selected_index(&visible) {
            self.play_or_pause(self.playlist[visible[pos]].id);
        }
    }

    /// Toca o item `id`. Se ele já for o atual, só alterna entre pausar e continuar,
    /// no mesmo mpv e sem voltar ao início.
    /// Nada acontece enquanto outro vídeo ainda está abrindo.
    pub fn play_or_pause(&mut self, id: u64) {
        if self.opening.is_some() {
            return;
        }
        let Some(entry) = self.playlist.iter().find(|e| e.id == id) else {
            return;
        };
        let r = if self.is_active(entry) {
            self.mpv.set_pause(!self.paused)
        } else if let Some(index) = self.pending_index(id) {
            let r = self.mpv.play_index(index);
            if r.is_ok() {
                self.opening = Some((id, Instant::now()));
            }
            r
        } else {
            return;
        };
        self.report(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn sync_keeps_queue_order() {
        // A fila foi reorganizada (c, a, b); o histórico perdeu o b e ganhou o d.
        let (gone, new) = sync_plan(&urls(&["c", "a", "b"]), None, urls(&["a", "c", "d"]), &[]);
        assert_eq!(gone, vec![2]);
        assert_eq!(new, urls(&["d"]));
    }

    #[test]
    fn sync_unchanged_does_nothing() {
        let (gone, new) = sync_plan(&urls(&["b", "a"]), None, urls(&["a", "b"]), &[]);
        assert!(gone.is_empty() && new.is_empty());
    }

    #[test]
    fn sync_removes_duplicates() {
        // O histórico tem a e b uma vez só; a fila ficou com cópias repetidas.
        let queue = urls(&["a", "b", "b", "a"]);
        let (gone, new) = sync_plan(&queue, None, urls(&["a", "b"]), &[]);
        assert_eq!(gone, vec![2, 3]);
        assert!(new.is_empty());
        // A cópia que está tocando é a que fica.
        let (gone, _) = sync_plan(&queue, Some(3), urls(&["a", "b"]), &[]);
        assert_eq!(gone, vec![0, 2]);
    }

    #[test]
    fn move_plan_targets() {
        let queue = [10, 11, 12, 13];
        // Para cima: o 12 vai para antes do 10.
        assert_eq!(move_plan(&queue, &queue, 12, 0), Some((2, 0)));
        // Para baixo: o 10 vai para depois do 12 (antes do 13).
        assert_eq!(move_plan(&queue, &queue, 10, 3), Some((0, 3)));
        // Para o fim.
        assert_eq!(move_plan(&queue, &queue, 11, 4), Some((1, 4)));
        // Soltar logo acima ou logo abaixo de si mesmo não muda nada.
        assert_eq!(move_plan(&queue, &queue, 11, 1), None);
        assert_eq!(move_plan(&queue, &queue, 11, 2), None);
        // Item que não está na fila.
        assert_eq!(move_plan(&queue, &queue, 99, 0), None);
    }

    #[test]
    fn move_plan_with_filter() {
        // Só o 10 e o 13 aparecem: soltar entre eles põe o 11 logo antes do 13.
        let queue = [10, 11, 12, 13];
        let visible = [10, 13];
        assert_eq!(move_plan(&queue, &visible, 11, 1), Some((1, 3)));
        // Depois do último visível.
        assert_eq!(move_plan(&queue, &visible, 11, 2), Some((1, 4)));
        // Sem itens visíveis, não há para onde mover.
        assert_eq!(move_plan(&queue, &[], 11, 0), None);
    }

    #[test]
    fn new_queue_follows_saved_order() {
        let order = urls(&["c", "a", "x"]);
        let (gone, new) = sync_plan(&[], None, urls(&["a", "b", "c", "d"]), &order);
        assert!(gone.is_empty());
        assert_eq!(new, urls(&["c", "a", "b", "d"]));
    }
}
