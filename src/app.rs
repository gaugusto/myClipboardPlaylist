use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::AppMsg;
use crate::cache::Cache;
use crate::cookies::Browser;
use crate::history::{self, HistorySource, Watcher};
use crate::metadata::{self, Fetcher, Metadata};
use crate::mpv::{Entry, Mpv, MpvEvent};
use crate::single_instance::Guard;
use crate::theme::{Palette, Theme, metrics};

const THUMB_SIZE: egui::Vec2 = egui::vec2(128.0, 72.0);
/// Thumbnail menor, para itens estreitos (abaixo de `NARROW_ROW`).
const THUMB_SIZE_SMALL: egui::Vec2 = egui::vec2(96.0, 54.0);
/// Largura de item abaixo da qual a thumbnail encolhe, para sobrar espaço ao título.
const NARROW_ROW: f32 = 520.0;
/// Largura de cada seletor (cookies, tema) no cabeçalho largo.
const PICKER_WIDTH: f32 = 170.0;
/// Largura mínima do botão Limpar.
const CLEAR_WIDTH: f32 = 120.0;
const PLAY_BUTTON: f32 = 38.0;
/// Chave do armazenamento do eframe onde fica o nome do tema escolhido.
const THEME_KEY: &str = "theme";
/// Chave do armazenamento: rodapé de atalhos visível ("true"/"false").
const SHOW_HELP_KEY: &str = "show_help";
/// Chave do armazenamento: navegador dos cookies do yt-dlp (vazio: nenhum).
const BROWSER_KEY: &str = "cookies_browser";
/// Chave do armazenamento: ordem da fila (JSON com as URLs), para refazê-la na mesma ordem
/// quando o mpv já fechou.
const ORDER_KEY: &str = "queue_order";
/// Distância da borda da lista em que arrastar um item a faz rolar.
const DRAG_SCROLL_EDGE: f32 = 48.0;
/// Prazo, em segundos, para confirmar o Limpar com um segundo clique.
const CONFIRM_CLEAR_SECS: f64 = 4.0;

struct Status {
    text: String,
    is_error: bool,
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

struct RowAction {
    play: bool,
    remove: bool,
    select: bool,
    /// Começou a arrastar o item.
    drag: bool,
    rect: egui::Rect,
}

enum MetaState {
    Loading,
    Ready(Metadata),
    Failed(String),
}

pub struct App {
    mpv: Mpv,
    tx: Sender<AppMsg>,
    rx: Receiver<AppMsg>,
    ctx: egui::Context,
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
    /// Momento (`egui::InputState::time`) do primeiro clique no Limpar, à espera da
    /// confirmação.
    confirm_clear: Option<f64>,
    playlist: Vec<Entry>,
    /// Ordem da fila (URLs), salva no armazenamento do eframe.
    order: Vec<String>,
    /// Id (no mpv) do item que está sendo arrastado para reorganizar a fila.
    dragging: Option<u64>,
    title: Option<String>,
    /// Títulos informados pelo mpv ao tocar, por URL (reserva se o yt-dlp falhar).
    titles: HashMap<String, String>,
    fetcher: Fetcher,
    /// Cache em disco dos metadados; esvaziado pelo comando Limpar.
    cache: Arc<Cache>,
    /// Metadados do yt-dlp (título, duração, thumbnail), por URL.
    meta: HashMap<String, MetaState>,
    status: Option<Status>,
    /// mpv ocioso (nada tocando); decide se ele continua aberto quando o app fecha.
    mpv_idle: bool,
    /// Links cuja remoção já foi pedida ao mpv, mas que ainda estão no espelho.
    removing: HashSet<u64>,
    /// Links cuja entrada na fila já foi pedida ao mpv, mas que ainda não estão no espelho.
    adding: HashSet<String>,
    /// O mpv foi fechado e a fila está sendo refeita num novo: até ela chegar, o espelho
    /// mantém a fila anterior (e ignora a playlist vazia do mpv novo).
    restoring: bool,
    /// Links sem vídeo removidos da fila desde a última leitura do histórico.
    not_videos: usize,
    /// Reprodução pausada no mpv (espelho da propriedade `pause`).
    paused: bool,
    filter: String,
    /// URL do item selecionado pelo teclado/clique (sobrevive a mudanças na fila).
    selected: Option<String>,
    /// Rola a lista até o item selecionado no próximo quadro.
    scroll_to_selected: bool,
    /// Rodapé com a ajuda de atalhos (Ctrl+H alterna).
    show_help: bool,
    /// Mantém o socket de instância única enquanto o app estiver aberto.
    _instance: Option<Guard>,
    themes: Vec<Theme>,
    theme: usize,
    /// Navegador de onde o yt-dlp (do app e do mpv) lê os cookies.
    browser: Option<Browser>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, mut instance: Option<Guard>) -> Self {
        let (tx, rx) = channel();
        if let Some(guard) = &mut instance {
            guard.listen(tx.clone(), cc.egui_ctx.clone());
        }
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let cache = Arc::new(Cache::load());
        let source: Arc<dyn HistorySource> = Arc::new(history::Dms);
        let watcher = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            Watcher::start(source.clone(), move || {
                let _ = tx.send(AppMsg::ClipboardChanged);
                ctx.request_repaint();
            })
        };
        let mut app = Self {
            fetcher: Fetcher::new(tx.clone(), cc.egui_ctx.clone(), cache.clone()),
            cache,
            meta: HashMap::new(),
            mpv: Mpv::new(tx.clone(), cc.egui_ctx.clone()),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            source,
            loading: false,
            refresh_again: false,
            generation: 0,
            editing: 0,
            edit_lock: Arc::new(Mutex::new(())),
            _watcher: watcher,
            confirm_clear: None,
            playlist: Vec::new(),
            order: Vec::new(),
            dragging: None,
            title: None,
            titles: HashMap::new(),
            status: None,
            mpv_idle: true,
            removing: HashSet::new(),
            adding: HashSet::new(),
            restoring: false,
            not_videos: 0,
            paused: false,
            filter: String::new(),
            selected: None,
            scroll_to_selected: false,
            show_help: true,
            _instance: instance,
            themes: Theme::builtin(),
            theme: 0,
            browser: None,
        };
        if let Some(show) = cc.storage.and_then(|s| s.get_string(SHOW_HELP_KEY)) {
            app.show_help = show != "false";
        }
        let saved = cc.storage.and_then(|s| s.get_string(THEME_KEY));
        if let Some(i) = saved.and_then(|name| {
            let name = Theme::migrate_name(&name);
            app.themes.iter().position(|t| t.name == name)
        }) {
            app.theme = i;
        }
        app.themes[app.theme].apply(&cc.egui_ctx);
        if let Some(order) = cc.storage.and_then(|s| s.get_string(ORDER_KEY)) {
            app.order = serde_json::from_str(&order).unwrap_or_default();
        }
        let browser = cc.storage.and_then(|s| s.get_string(BROWSER_KEY));
        app.set_browser(browser.as_deref().and_then(Browser::from_name));
        if let Some(playlist) = app.mpv.attach() {
            app.playlist = playlist;
            app.request_metadata();
            app.status = Status::info("Reconectado ao mpv que continuou tocando.");
        }
        app.refresh();
        app
    }

    fn report(&mut self, result: anyhow::Result<()>) {
        if let Err(e) = result {
            self.status = Status::error(format!("{e:#}"));
        }
    }

    /// Lê o histórico do clipboard em segundo plano.
    fn refresh(&mut self) {
        if self.loading {
            self.refresh_again = true;
            return;
        }
        self.loading = true;
        let (tx, ctx, source) = (self.tx.clone(), self.ctx.clone(), self.source.clone());
        let generation = self.generation;
        thread::spawn(move || {
            let result = history::fetch_links(source.as_ref()).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppMsg::History(generation, result));
            ctx.request_repaint();
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
        let (tx, ctx, source) = (self.tx.clone(), self.ctx.clone(), self.source.clone());
        let lock = self.edit_lock.clone();
        thread::spawn(move || {
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let result = edit(source.as_ref()).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppMsg::HistoryEdited(result));
            ctx.request_repaint();
        });
    }

    /// Comando Limpar (botão ou Ctrl+L, repetido para confirmar): esvazia a fila, o
    /// histórico do clipboard e o cache de metadados.
    fn clear_command(&mut self) {
        let now = self.ctx.input(|i| i.time);
        if !self.clear_armed(now) {
            self.confirm_clear = Some(now);
            self.ctx
                .request_repaint_after(Duration::from_secs_f64(CONFIRM_CLEAR_SECS));
            return;
        }
        self.confirm_clear = None;
        let stopped = self.mpv.clear();
        self.report(stopped);
        let cleared = self.cache.clear();
        self.report(cleared);
        self.meta.clear();
        self.titles.clear();
        self.ctx.forget_all_images();
        self.edit_history(|source| {
            source.clear()?;
            Ok(Some(
                "Fila, histórico do clipboard e cache limpos.".to_owned(),
            ))
        });
    }

    /// O Limpar já foi clicado uma vez e espera a confirmação.
    fn clear_armed(&self, now: f64) -> bool {
        self.confirm_clear
            .is_some_and(|t| now - t < CONFIRM_CLEAR_SECS)
    }

    /// Remove o item da fila e também do histórico do clipboard e do cache. As outras
    /// cópias do link, se houver, saem na próxima leitura do histórico.
    fn remove_entry(&mut self, id: u64) {
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
        if let Some(uri) = self.thumb_uri(url) {
            self.ctx.forget_image(&uri);
        }
        self.meta.remove(url);
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

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
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
                AppMsg::Activate => {
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                AppMsg::Metadata(url, result) => {
                    let not_video = result.as_ref().is_err_and(|e| metadata::is_not_video(e));
                    let state = result.map_or_else(MetaState::Failed, MetaState::Ready);
                    self.meta.insert(url.clone(), state);
                    if not_video {
                        self.remove_not_video(&url);
                    }
                }
                AppMsg::Mpv(event) => match event {
                    // A playlist vazia do mpv novo chegaria antes da fila refeita.
                    MpvEvent::Playlist(list) if self.restoring && list.is_empty() => {}
                    MpvEvent::Playlist(list) => {
                        self.restoring = false;
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
                    MpvEvent::Idle(idle) => self.mpv_idle = idle,
                    MpvEvent::Pause(paused) => self.paused = paused,
                    MpvEvent::FileError(e) => self.status = Status::error(e),
                    MpvEvent::Exited => self.restore_queue(),
                },
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

    fn entry_title(&self, entry: &Entry) -> String {
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
    fn entry_details(&self, entry: &Entry) -> String {
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

    fn palette(&self) -> &Palette {
        &self.themes[self.theme].palette
    }

    /// Endereço da imagem da thumbnail de `url`: prefere a cópia local (cache) à URL remota.
    fn thumb_uri(&self, url: &str) -> Option<String> {
        match self.meta.get(url) {
            Some(MetaState::Ready(Metadata {
                thumb_file: Some(file),
                ..
            })) => Some(format!("file://{}", file.display())),
            Some(MetaState::Ready(Metadata {
                thumbnail: Some(url),
                ..
            })) => Some(url.clone()),
            _ => None,
        }
    }

    fn thumbnail(&self, ui: &mut egui::Ui, entry: &Entry, size: egui::Vec2) {
        let p = self.palette();
        let meta = self.meta.get(&entry.filename);
        let radius = metrics::RADIUS_SM;
        let rect = if let Some(source) = self.thumb_uri(&entry.filename) {
            let image = egui::Image::new(source)
                .fit_to_exact_size(size)
                .corner_radius(radius);
            ui.add(image).rect
        } else {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, radius, p.sunken);
            if matches!(meta, Some(MetaState::Loading) | None) {
                ui.put(rect, egui::Spinner::new().color(p.text_muted));
            } else {
                ui.put(
                    rect,
                    egui::Label::new(egui::RichText::new("🎞").size(24.0).color(p.text_muted)),
                );
            }
            rect
        };
        // Selo de duração sobre a thumbnail, como nos players de vídeo.
        if let Some(badge) = self.duration_badge(entry) {
            let font = egui::FontId::proportional(11.0);
            let galley = ui
                .painter()
                .layout_no_wrap(badge, font, egui::Color32::WHITE);
            let pad = egui::vec2(5.0, 2.0);
            let max = rect.right_bottom() - egui::vec2(4.0, 4.0);
            let bg = egui::Rect::from_min_max(max - galley.size() - pad * 2.0, max);
            ui.painter()
                .rect_filled(bg, 4, egui::Color32::from_black_alpha(190));
            ui.painter()
                .galley(bg.min + pad, galley, egui::Color32::WHITE);
        }
    }

    /// Número do item na fila (a posição no mpv, mesmo com filtro), à esquerda da thumbnail.
    /// A largura acompanha o maior número da fila, para os itens ficarem alinhados.
    fn position(&self, ui: &mut egui::Ui, index: usize, entry: &Entry, height: f32) {
        let p = self.palette();
        let font = egui::FontId::proportional(18.0);
        let digits = self.playlist.len().max(1).to_string().len();
        let widest = ui
            .painter()
            .layout_no_wrap("0".repeat(digits), font.clone(), p.text_muted)
            .size()
            .x;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(widest, height), egui::Sense::hover());
        let color = if entry.current {
            p.accent
        } else {
            p.text_muted
        };
        ui.painter().text(
            rect.right_center(),
            egui::Align2::RIGHT_CENTER,
            (index + 1).to_string(),
            font,
            color,
        );
    }

    fn duration_badge(&self, entry: &Entry) -> Option<String> {
        match self.meta.get(&entry.filename) {
            Some(MetaState::Ready(m)) if m.is_live => Some("AO VIVO".to_owned()),
            Some(MetaState::Ready(m)) => m.duration.map(metadata::format_duration),
            _ => None,
        }
    }

    /// Desenha um item da fila.
    fn row(&self, ui: &mut egui::Ui, index: usize, entry: &Entry, selected: bool) -> RowAction {
        let p = *self.palette();
        let (mut play, mut remove) = (false, false);
        let builder = egui::UiBuilder::new()
            .id_salt(("row", index))
            .sense(egui::Sense::click_and_drag());
        let response = ui
            .scope_builder(builder, |ui| {
                if self.dragging == Some(entry.id) {
                    ui.multiply_opacity(0.45);
                }
                let hovered = ui.response().hovered() && self.dragging.is_none();
                let fill = if entry.current {
                    p.surface_active
                } else if selected || hovered {
                    p.surface_hover
                } else {
                    p.surface
                };
                let stroke = if selected {
                    egui::Stroke::new(1.5, p.accent)
                } else if entry.current {
                    egui::Stroke::new(1.0, p.accent.gamma_multiply(0.5))
                } else {
                    egui::Stroke::new(1.0, p.border)
                };
                let card = egui::Frame::new()
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(metrics::RADIUS_LG)
                    .inner_margin(10);
                card.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let thumb = if ui.available_width() < NARROW_ROW {
                            THUMB_SIZE_SMALL
                        } else {
                            THUMB_SIZE
                        };
                        self.position(ui, index, entry, thumb.y);
                        self.thumbnail(ui, entry, thumb);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            remove = self
                                .remove_button(ui, hovered || selected || entry.current)
                                .clicked();
                            play = self.play_button(ui, entry).clicked();
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.add_space(6.0);
                                let title = egui::RichText::new(self.entry_title(entry))
                                    .strong()
                                    .size(15.0);
                                ui.add(egui::Label::new(title).truncate().selectable(false));
                                ui.horizontal(|ui| {
                                    if entry.current {
                                        self.pill(
                                            ui,
                                            if self.paused { "PAUSADO" } else { "TOCANDO" },
                                        );
                                    }
                                    let details = egui::RichText::new(self.entry_details(entry))
                                        .color(p.text_muted)
                                        .size(12.5);
                                    ui.add(egui::Label::new(details).truncate().selectable(false));
                                });
                            });
                        });
                    });
                });
            })
            .response;
        let mut hover = format!(
            "{}\n{}\nClique duplo para {}; arraste para mudar a posição na fila",
            self.entry_title(entry),
            entry.filename,
            if self.is_playing(entry) {
                "pausar"
            } else {
                "tocar"
            }
        );
        if let Some(MetaState::Failed(e)) = self.meta.get(&entry.filename) {
            hover.push_str(&format!("\n\nyt-dlp: {e}"));
        }
        let response = if self.dragging.is_some() {
            response
        } else {
            response.on_hover_text(hover)
        };
        if selected && self.scroll_to_selected {
            response.scroll_to_me(None);
        }
        RowAction {
            play: play || response.double_clicked(),
            remove,
            select: response.clicked(),
            drag: response.drag_started(),
            rect: response.rect,
        }
    }

    /// O item é o atual do mpv e não está pausado.
    fn is_playing(&self, entry: &Entry) -> bool {
        entry.current && !self.mpv_idle && !self.paused
    }

    /// Botão circular de tocar/pausar na cor de destaque. Mostra pausa enquanto o
    /// item estiver tocando.
    fn play_button(&self, ui: &mut egui::Ui, entry: &Entry) -> egui::Response {
        let p = self.palette();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(PLAY_BUTTON, PLAY_BUTTON), egui::Sense::click());
        let fill = if response.hovered() {
            p.accent_hover
        } else {
            p.accent
        };
        let painter = ui.painter();
        painter.circle_filled(rect.center(), PLAY_BUTTON / 2.0, fill);
        let playing = self.is_playing(entry);
        if playing {
            // Duas barras verticais.
            let (w, h, gap) = (PLAY_BUTTON * 0.11, PLAY_BUTTON * 0.4, PLAY_BUTTON * 0.08);
            for dx in [-(gap / 2.0 + w / 2.0), gap / 2.0 + w / 2.0] {
                let bar = egui::Rect::from_center_size(
                    rect.center() + egui::vec2(dx, 0.0),
                    egui::vec2(w, h),
                );
                painter.rect_filled(bar, 1.0, p.on_accent);
            }
        } else {
            // Triângulo levemente deslocado para a direita, para parecer centralizado.
            let c = rect.center() + egui::vec2(2.0, 0.0);
            let r = PLAY_BUTTON * 0.22;
            let points = vec![
                c + egui::vec2(-r * 0.8, -r),
                c + egui::vec2(-r * 0.8, r),
                c + egui::vec2(r, 0.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                p.on_accent,
                egui::Stroke::NONE,
            ));
        }
        let hint = if playing {
            "Pausar"
        } else if entry.current && !self.mpv_idle {
            "Continuar"
        } else {
            "Tocar"
        };
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hint)
    }

    /// Botão discreto de remover (um X desenhado), que ganha cor quando o cartão está em foco.
    fn remove_button(&self, ui: &mut egui::Ui, visible: bool) -> egui::Response {
        let p = self.palette();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
        let color = if response.hovered() {
            ui.painter()
                .circle_filled(rect.center(), 15.0, p.danger.gamma_multiply(0.15));
            p.danger
        } else if visible {
            p.text_muted
        } else {
            p.text_muted.gamma_multiply(0.5)
        };
        let (c, r) = (rect.center(), 5.0);
        let stroke = egui::Stroke::new(1.8, color);
        ui.painter()
            .line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke);
        ui.painter()
            .line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke);
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Remover da fila e do histórico do clipboard")
    }

    /// Etiqueta pequena na cor de destaque.
    fn pill(&self, ui: &mut egui::Ui, text: &str) {
        let p = self.palette();
        egui::Frame::new()
            .fill(p.accent.gamma_multiply(0.18))
            .corner_radius(metrics::RADIUS_SM)
            .inner_margin(egui::Margin::symmetric(6, 1))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(text)
                        .color(p.accent)
                        .size(10.5)
                        .strong(),
                );
            });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let spacing = ui.spacing().item_spacing.x;
        let title_width = ui
            .painter()
            .layout_no_wrap(
                "Clipboard Playlist".to_owned(),
                egui::TextStyle::Heading.resolve(ui.style()),
                p.text,
            )
            .size()
            .x;
        let controls_width = 2.0 * PICKER_WIDTH + CLEAR_WIDTH + 3.0 * spacing;
        // Janela estreita: os seletores descem para uma linha própria, dividindo a largura.
        let wide = ui.available_width() >= title_width + controls_width + 24.0;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(egui::RichText::new("Clipboard Playlist").heading().strong());
                let total = self.playlist.len();
                let count = match total {
                    0 => "Fila vazia".to_owned(),
                    1 => "1 vídeo na fila".to_owned(),
                    n => format!("{n} vídeos na fila"),
                };
                let count = if self.filter.trim().is_empty() || total == 0 {
                    count
                } else {
                    format!("{} de {count}", self.visible().len())
                };
                ui.label(
                    egui::RichText::new(format!("{count}  ·  fonte: {}", self.source.name()))
                        .color(p.text_muted)
                        .size(12.5),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.clear_button(ui);
                if wide {
                    self.theme_picker(ui, PICKER_WIDTH);
                    self.browser_picker(ui, PICKER_WIDTH);
                }
            });
        });
        if !wide {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let width = (ui.available_width() - spacing) / 2.0;
                self.browser_picker(ui, width);
                self.theme_picker(ui, width);
            });
        }
        ui.add_space(12.0);
        self.filter_field(ui);
        if let Some(status) = &self.status {
            ui.add_space(12.0);
            let (color, fill, icon) = if status.is_error {
                (p.danger, p.danger.gamma_multiply(0.12), "⚠")
            } else {
                (p.text_muted, p.surface, "ℹ")
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(metrics::RADIUS_MD)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let text = egui::RichText::new(format!("{icon}   {}", status.text))
                        .color(color)
                        .size(12.5);
                    ui.add(egui::Label::new(text).truncate());
                });
        }
    }

    fn clear_button(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let armed = self.clear_armed(ui.input(|i| i.time));
        let (label, color, fill) = if armed {
            ("Confirmar limpeza", p.on_accent, p.danger)
        } else {
            ("Limpar", p.danger, p.surface)
        };
        let button = egui::Button::new(egui::RichText::new(label).color(color).strong())
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, p.danger.gamma_multiply(0.5)))
            .corner_radius(metrics::RADIUS_MD)
            .min_size(egui::vec2(CLEAR_WIDTH, 34.0));
        let hint = format!(
            "Exclui todos os vídeos da fila, apaga o histórico do clipboard ({}) e o cache. \
             Clique de novo para confirmar. Atalho: Ctrl+L",
            self.source.name()
        );
        if ui.add(button).on_hover_text(hint).clicked() {
            self.clear_command();
        }
    }

    fn theme_picker(&mut self, ui: &mut egui::Ui, width: f32) {
        let mut selected = self.theme;
        egui::ComboBox::from_id_salt("theme")
            .selected_text(format!("🎨  {}", self.themes[self.theme].name))
            .width(width)
            .show_ui(ui, |ui| {
                for (i, theme) in self.themes.iter().enumerate() {
                    ui.selectable_value(&mut selected, i, theme.name);
                }
            });
        if selected != self.theme {
            self.set_theme(ui.ctx(), selected);
        }
    }

    fn browser_picker(&mut self, ui: &mut egui::Ui, width: f32) {
        let mut selected = self.browser;
        let name = |b: Option<Browser>| b.map_or("nenhum", Browser::name);
        egui::ComboBox::from_id_salt("browser")
            .selected_text(format!("Cookies: {}", name(self.browser)))
            .width(width)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut selected, None, name(None));
                for browser in Browser::ALL {
                    ui.selectable_value(&mut selected, Some(browser), browser.name());
                }
            })
            .response
            .on_hover_text(
                "Navegador de onde o yt-dlp lê os cookies, para vídeos que pedem login \
                 (\"Sign in to confirm you're not a bot\").",
            );
        if selected != self.browser {
            self.set_browser(selected);
            // As consultas que falharam (talvez por falta de login) são refeitas.
            self.meta.retain(|_, m| !matches!(m, MetaState::Failed(_)));
            self.request_metadata();
        }
    }

    fn set_browser(&mut self, browser: Option<Browser>) {
        self.browser = browser;
        self.fetcher.set_browser(browser);
        let applied = self.mpv.set_cookies(browser.map(Browser::ytdlp_arg));
        self.report(applied);
    }

    fn set_theme(&mut self, ctx: &egui::Context, index: usize) {
        self.theme = index;
        self.themes[index].apply(ctx);
    }

    fn empty_state(&self, ui: &mut egui::Ui) {
        let p = self.palette();
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(egui::RichText::new("📋").size(42.0).color(p.text_muted));
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Nenhum vídeo na fila")
                    .size(17.0)
                    .strong(),
            );
            ui.label(
                egui::RichText::new("Copie links de vídeos: eles entram na fila sozinhos.")
                    .color(p.text_muted),
            );
        });
    }

    fn filter_field(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let edit = egui::TextEdit::singleline(&mut self.filter)
            .id(egui::Id::new("filter"))
            .hint_text(egui::RichText::new("🔍  Filtrar por título ou site…").color(p.text_muted))
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(12, 8))
            .background_color(p.surface)
            .font(egui::TextStyle::Body);
        let response = ui.add(edit);
        // Setas e Esc ficam com o campo, senão o egui as usaria para mover/soltar o foco.
        // (Os atalhos já foram consumidos em `handle_keys`.)
        let filter = egui::EventFilter {
            tab: false,
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
        };
        ui.memory_mut(|m| m.set_focus_lock_filter(response.id, filter));
        // Foco persistente: digitar sempre filtra, sem precisar clicar no campo.
        if !response.has_focus() {
            response.request_focus();
        }
        if response.changed() {
            self.selected = self
                .visible()
                .first()
                .map(|&i| self.playlist[i].filename.clone());
            self.scroll_to_selected = true;
        }
    }

    /// Índices (na playlist) dos itens que passam pelo filtro.
    /// Cada palavra do filtro precisa aparecer no título ou na URL.
    fn visible(&self) -> Vec<usize> {
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

    /// Move a seleção `delta` posições dentro da lista filtrada.
    fn move_selection(&mut self, delta: isize) {
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

    fn play_selected(&mut self) {
        let visible = self.visible();
        if let Some(pos) = self.selected_index(&visible) {
            self.play_or_pause(visible[pos]);
        }
    }

    /// Toca o item `index`. Se ele já for o atual, só alterna entre pausar e continuar,
    /// no mesmo mpv e sem voltar ao início.
    fn play_or_pause(&mut self, index: usize) {
        let Some(entry) = self.playlist.get(index) else {
            return;
        };
        let r = if entry.current && !self.mpv_idle {
            self.mpv.set_pause(!self.paused)
        } else if let Some(index) = self.pending_index(entry.id) {
            self.mpv.play_index(index)
        } else {
            return;
        };
        self.report(r);
    }

    /// Atalhos de teclado. As teclas são consumidas antes de o campo de filtro
    /// ser desenhado, para que ele (sempre em foco) não as intercepte.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        use egui::{Key, Modifiers};
        let (down, up, enter, clear_all, escape, help, next_theme) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown)
                    | i.consume_key(Modifiers::CTRL, Key::J),
                i.consume_key(Modifiers::NONE, Key::ArrowUp)
                    | i.consume_key(Modifiers::CTRL, Key::K),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::CTRL, Key::L),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::CTRL, Key::H),
                i.consume_key(Modifiers::CTRL, Key::T),
            )
        });
        if down {
            self.move_selection(1);
        }
        if up {
            self.move_selection(-1);
        }
        if enter {
            self.play_selected();
        }
        if clear_all {
            self.clear_command();
        }
        if help {
            self.show_help = !self.show_help;
        }
        if next_theme {
            self.set_theme(ui.ctx(), (self.theme + 1) % self.themes.len());
        }
        // Esc cancela o Limpar e limpa o filtro; com o filtro já vazio, fecha a janela.
        if escape {
            if self.clear_armed(ui.input(|i| i.time)) {
                self.confirm_clear = None;
            } else if self.filter.is_empty() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.filter.clear();
                self.scroll_to_selected = true;
            }
        }
    }

    fn footer(&self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let hint = |ui: &mut egui::Ui, keys: &[&str], text: &str| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for key in keys {
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .corner_radius(4)
                        .inner_margin(egui::Margin::symmetric(5, 1))
                        .show(ui, |ui| {
                            // Monoespaçada: a fonte proporcional padrão não tem as setas ↑ ↓.
                            ui.label(
                                egui::RichText::new(*key)
                                    .monospace()
                                    .size(11.0)
                                    .color(p.text),
                            );
                        });
                }
                ui.add_space(3.0);
                ui.label(egui::RichText::new(text).size(11.5).color(p.text_muted));
            });
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(14.0, 6.0);
            hint(ui, &["↑", "↓", "Ctrl+J", "Ctrl+K"], "navegar");
            hint(ui, &["Enter"], "tocar/pausar");
            hint(ui, &["Ctrl+L"], "limpar tudo");
            hint(ui, &["Esc"], "limpar filtro / fechar");
            hint(ui, &["Ctrl+T"], "trocar tema");
            hint(ui, &["Ctrl+H"], "ocultar ajuda");
        });
    }

    fn no_matches(&self, ui: &mut egui::Ui) {
        let p = self.palette();
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(egui::RichText::new("🔍").size(36.0).color(p.text_muted));
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Nenhum vídeo corresponde a “{}”",
                    self.filter.trim()
                ))
                .size(16.0)
                .strong(),
            );
            ui.label(egui::RichText::new("Esc limpa o filtro.").color(p.text_muted));
        });
    }

    fn playlist(&mut self, ui: &mut egui::Ui) {
        if self.playlist.is_empty() {
            self.empty_state(ui);
            return;
        }
        let visible = self.visible();
        if visible.is_empty() {
            self.no_matches(ui);
            return;
        }
        // Mantém sempre um item selecionado entre os visíveis.
        if self.selected_index(&visible).is_none() {
            self.selected = Some(self.playlist[visible[0]].filename.clone());
        }
        let mut play = None;
        let mut remove = None;
        let mut select = None;
        let mut drag = None;
        // Id e retângulo de cada item visível, na ordem da tela.
        let mut rows: Vec<(u64, egui::Rect)> = Vec::new();
        let viewport = egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = metrics::GAP;
                self.drag_autoscroll(ui);
                for &i in &visible {
                    let entry = &self.playlist[i];
                    let is_selected = self.selected.as_deref() == Some(entry.filename.as_str());
                    let action = self.row(ui, i, entry, is_selected);
                    rows.push((entry.id, action.rect));
                    if action.drag {
                        drag = Some(entry.id);
                    }
                    if action.play {
                        play = Some(i);
                    }
                    if action.remove {
                        remove = Some(i);
                    }
                    if action.select || action.play || action.drag {
                        select = Some(entry.filename.clone());
                    }
                }
                ui.add_space(metrics::GAP);
            })
            .inner_rect;
        self.scroll_to_selected = false;
        if drag.is_some() {
            self.dragging = drag;
        }
        self.drag_and_drop(ui, viewport, &rows);
        if select.is_some() {
            self.selected = select;
        }
        if let Some(i) = play {
            self.play_or_pause(i);
        }
        if let Some(i) = remove {
            self.remove_entry(self.playlist[i].id);
        }
    }

    /// Rola a lista enquanto um item é arrastado perto da borda de cima ou de baixo.
    fn drag_autoscroll(&self, ui: &mut egui::Ui) {
        let Some(pointer) = self.dragging.and(ui.ctx().pointer_interact_pos()) else {
            return;
        };
        let clip = ui.clip_rect();
        let speed = if pointer.y < clip.top() + DRAG_SCROLL_EDGE {
            10.0
        } else if pointer.y > clip.bottom() - DRAG_SCROLL_EDGE {
            -10.0
        } else {
            return;
        };
        ui.scroll_with_delta(egui::vec2(0.0, speed));
        ui.ctx().request_repaint();
    }

    /// Enquanto um item é arrastado, mostra onde ele vai cair; ao soltar, move-o no mpv.
    /// `rows` são os itens visíveis, na ordem da tela.
    fn drag_and_drop(&mut self, ui: &egui::Ui, viewport: egui::Rect, rows: &[(u64, egui::Rect)]) {
        let Some(dragged) = self.dragging else {
            return;
        };
        let (pointer, down) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_down()));
        // Posição de destino: quantos itens visíveis ficam acima do ponteiro.
        let slot = pointer.map(|p| rows.iter().filter(|(_, r)| r.center().y < p.y).count());
        if !down {
            self.dragging = None;
            if let Some(slot) = slot {
                self.drop_entry(dragged, slot, rows);
            }
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let (Some(slot), Some((_, first))) = (slot, rows.first()) else {
            return;
        };
        let y = match slot.checked_sub(1).and_then(|i| rows.get(i)) {
            Some((_, above)) => above.bottom() + metrics::GAP / 2.0,
            None => first.top() - metrics::GAP / 2.0,
        };
        let x = first.x_range();
        ui.painter_at(viewport.expand(2.0)).hline(
            x,
            y,
            egui::Stroke::new(3.0, self.palette().accent),
        );
    }

    /// Move `url` para a posição `slot` entre os itens visíveis (`rows`). Com o filtro
    /// ativo, ele fica logo antes do item visível que ocupava essa posição.
    fn drop_entry(&mut self, id: u64, slot: usize, rows: &[(u64, egui::Rect)]) {
        let Some(from) = self.pending_index(id) else {
            return;
        };
        let to = match rows.get(slot) {
            Some(&(before, _)) => self.pending_index(before),
            None => rows
                .last()
                .and_then(|&(last, _)| self.pending_index(last))
                .map(|i| i + 1),
        };
        let Some(to) = to else {
            return;
        };
        // Soltar no mesmo lugar.
        if to == from || to == from + 1 {
            return;
        }
        let moved = self.mpv.move_entry(from, to);
        self.report(moved);
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(THEME_KEY, self.themes[self.theme].name.to_owned());
        storage.set_string(SHOW_HELP_KEY, self.show_help.to_string());
        if let Ok(order) = serde_json::to_string(&self.order) {
            storage.set_string(ORDER_KEY, order);
        }
        storage.set_string(
            BROWSER_KEY,
            self.browser.map_or("", Browser::name).to_owned(),
        );
    }

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Ao voltar para a janela, relê o histórico: ele pode ter sido editado fora do app.
        if ui.input(|i| i.events.contains(&egui::Event::WindowFocused(true))) {
            self.refresh();
        }
        self.handle_keys(ui);
        let bg = self.palette().background;
        let m = metrics::PAGE_MARGIN;
        egui::Panel::top("header")
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(bg).inner_margin(egui::Margin {
                left: m,
                right: m,
                top: m + 2,
                bottom: 14,
            }))
            .show(ui, |ui| self.header(ui));
        if self.show_help {
            egui::Panel::bottom("footer")
                .frame(
                    egui::Frame::new()
                        .fill(bg)
                        .inner_margin(egui::Margin::symmetric(m, 8)),
                )
                .show(ui, |ui| self.footer(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::Frame::new()
                .fill(bg)
                .inner_margin(egui::Margin {
                    left: m,
                    right: m,
                    top: 4,
                    bottom: 0,
                })
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.playlist(ui);
                });
        });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.mpv.detach(self.mpv_idle);
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
    fn new_queue_follows_saved_order() {
        let order = urls(&["c", "a", "x"]);
        let (gone, new) = sync_plan(&[], None, urls(&["a", "b", "c", "d"]), &order);
        assert!(gone.is_empty());
        assert_eq!(new, urls(&["c", "a", "b", "d"]));
    }
}
