use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use eframe::egui;

use crate::AppMsg;
use crate::cache::Cache;
use crate::history::{self, HistorySource};
use crate::metadata::{self, Fetcher, Metadata};
use crate::mpv::{Entry, Mpv, MpvEvent};
use crate::single_instance::Guard;
use crate::theme::{Palette, Theme, metrics};

const THUMB_SIZE: egui::Vec2 = egui::vec2(128.0, 72.0);
const PLAY_BUTTON: f32 = 38.0;
/// Chave do armazenamento do eframe onde fica o nome do tema escolhido.
const THEME_KEY: &str = "theme";
/// Chave do armazenamento: rodapé de atalhos visível ("true"/"false").
const SHOW_HELP_KEY: &str = "show_help";

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

struct RowAction {
    play: bool,
    remove: bool,
    select: bool,
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
    /// A leitura em andamento veio do comando Atualizar (e não da abertura do app).
    manual_refresh: bool,
    playlist: Vec<Entry>,
    title: Option<String>,
    /// Títulos informados pelo mpv ao tocar, por URL (reserva se o yt-dlp falhar).
    titles: HashMap<String, String>,
    fetcher: Fetcher,
    /// Cache em disco dos metadados; esvaziado pelo comando Atualizar.
    cache: Arc<Cache>,
    /// Metadados do yt-dlp (título, duração, thumbnail), por URL.
    meta: HashMap<String, MetaState>,
    status: Option<Status>,
    /// mpv ocioso (nada tocando); decide se ele continua aberto quando o app fecha.
    mpv_idle: bool,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, mut instance: Option<Guard>) -> Self {
        let (tx, rx) = channel();
        if let Some(guard) = &mut instance {
            guard.listen(tx.clone(), cc.egui_ctx.clone());
        }
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let cache = Arc::new(Cache::load());
        let mut app = Self {
            fetcher: Fetcher::new(tx.clone(), cc.egui_ctx.clone(), cache.clone()),
            cache,
            meta: HashMap::new(),
            mpv: Mpv::new(tx.clone(), cc.egui_ctx.clone()),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            source: Arc::new(history::Dms),
            loading: false,
            manual_refresh: false,
            playlist: Vec::new(),
            title: None,
            titles: HashMap::new(),
            status: None,
            mpv_idle: true,
            paused: false,
            filter: String::new(),
            selected: None,
            scroll_to_selected: false,
            show_help: true,
            _instance: instance,
            themes: Theme::builtin(),
            theme: 0,
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
            return;
        }
        self.loading = true;
        let (tx, ctx, source) = (self.tx.clone(), self.ctx.clone(), self.source.clone());
        thread::spawn(move || {
            let result = history::fetch_links(source.as_ref()).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppMsg::History(result));
            ctx.request_repaint();
        });
    }

    /// Comando Atualizar (botão ou Ctrl+R): esvazia o cache de metadados, que voltam
    /// a ser consultados no yt-dlp, e relê o histórico do clipboard.
    fn refresh_command(&mut self) {
        if self.loading {
            return;
        }
        let cleared = self.cache.clear();
        self.report(cleared);
        self.meta.clear();
        self.ctx.forget_all_images();
        self.request_metadata();
        self.refresh();
        self.manual_refresh = true;
    }

    /// Substitui a fila pelos links do clipboard, a menos que não haja nenhum.
    fn apply_history(&mut self, result: Result<Vec<String>, String>) {
        self.loading = false;
        let manual = std::mem::take(&mut self.manual_refresh);
        let links = match result {
            Ok(links) => links,
            Err(e) => {
                self.status = Status::error(e);
                return;
            }
        };
        if links.is_empty() {
            // Ao abrir o app, um clipboard sem links não merece aviso.
            if manual {
                self.status = Status::info(format!(
                    "Nenhum link válido no clipboard ({}); fila mantida.",
                    self.source.name()
                ));
            }
            return;
        }
        let current = self
            .playlist
            .iter()
            .find(|e| e.current)
            .map(|e| e.filename.clone());
        let result = self.mpv.replace(&links, current.as_deref());
        self.status = if result.is_ok() {
            Status::info(format!("Fila atualizada: {} link(s).", links.len()))
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
                AppMsg::History(result) => self.apply_history(result),
                AppMsg::Activate => {
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                AppMsg::Metadata(url, result) => {
                    let state = result.map_or_else(MetaState::Failed, MetaState::Ready);
                    self.meta.insert(url, state);
                }
                AppMsg::Mpv(event) => match event {
                    MpvEvent::Playlist(list) => {
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
                    MpvEvent::Exited => {
                        self.mpv_idle = true;
                        self.paused = false;
                        self.playlist.clear();
                        self.title = None;
                    }
                },
            }
        }
    }

    /// Busca no cache, ou pede ao yt-dlp, os metadados dos itens da fila que ainda não
    /// foram consultados.
    fn request_metadata(&mut self) {
        for entry in &self.playlist {
            if self.meta.contains_key(&entry.filename) {
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

    fn thumbnail(&self, ui: &mut egui::Ui, entry: &Entry) {
        let p = self.palette();
        let meta = self.meta.get(&entry.filename);
        let radius = metrics::RADIUS_SM;
        // Prefere a cópia local (cache) à URL remota.
        let source = match meta {
            Some(MetaState::Ready(Metadata {
                thumb_file: Some(file),
                ..
            })) => Some(format!("file://{}", file.display())),
            Some(MetaState::Ready(Metadata {
                thumbnail: Some(url),
                ..
            })) => Some(url.clone()),
            _ => None,
        };
        let rect = if let Some(source) = source {
            let image = egui::Image::new(source)
                .fit_to_exact_size(THUMB_SIZE)
                .corner_radius(radius);
            ui.add(image).rect
        } else {
            let (rect, _) = ui.allocate_exact_size(THUMB_SIZE, egui::Sense::hover());
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
            .sense(egui::Sense::click());
        let response = ui
            .scope_builder(builder, |ui| {
                let hovered = ui.response().hovered();
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
                        self.thumbnail(ui, entry);
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
            "{}\n{}\nClique duplo para {}",
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
        let response = response.on_hover_text(hover);
        if selected && self.scroll_to_selected {
            response.scroll_to_me(None);
        }
        RowAction {
            play: play || response.double_clicked(),
            remove,
            select: response.clicked(),
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
            .on_hover_text("Remover da fila")
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
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(
                    egui::RichText::new("myClipboardPlayList")
                        .heading()
                        .strong(),
                );
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
                self.refresh_button(ui);
                self.theme_picker(ui);
            });
        });
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

    fn refresh_button(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let label = if self.loading {
            "Atualizando…"
        } else {
            "⟳  Atualizar"
        };
        let button = egui::Button::new(egui::RichText::new(label).color(p.on_accent).strong())
            .fill(p.accent)
            .corner_radius(metrics::RADIUS_MD)
            .min_size(egui::vec2(120.0, 34.0));
        let hint = format!(
            "Substitui a fila pelos links do histórico do clipboard ({}). Atalho: Ctrl+R",
            self.source.name()
        );
        if ui
            .add_enabled(!self.loading, button)
            .on_hover_text(hint)
            .clicked()
        {
            self.refresh_command();
        }
    }

    fn theme_picker(&mut self, ui: &mut egui::Ui) {
        let mut selected = self.theme;
        egui::ComboBox::from_id_salt("theme")
            .selected_text(format!("🎨  {}", self.themes[self.theme].name))
            .width(170.0)
            .show_ui(ui, |ui| {
                for (i, theme) in self.themes.iter().enumerate() {
                    ui.selectable_value(&mut selected, i, theme.name);
                }
            });
        if selected != self.theme {
            self.set_theme(ui.ctx(), selected);
        }
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
                egui::RichText::new("Copie links de vídeos e clique em Atualizar.")
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
        let r = match self.playlist.get(index) {
            Some(entry) if entry.current && !self.mpv_idle => self.mpv.set_pause(!self.paused),
            _ => self.mpv.play_index(index),
        };
        self.report(r);
    }

    /// Atalhos de teclado. As teclas são consumidas antes de o campo de filtro
    /// ser desenhado, para que ele (sempre em foco) não as intercepte.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        use egui::{Key, Modifiers};
        let (down, up, enter, refresh, clear, help, next_theme) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown)
                    | i.consume_key(Modifiers::CTRL, Key::J),
                i.consume_key(Modifiers::NONE, Key::ArrowUp)
                    | i.consume_key(Modifiers::CTRL, Key::K),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::CTRL, Key::R),
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
        if refresh {
            self.refresh_command();
        }
        if help {
            self.show_help = !self.show_help;
        }
        if next_theme {
            self.set_theme(ui.ctx(), (self.theme + 1) % self.themes.len());
        }
        // Esc limpa o filtro; com o filtro já vazio, fecha a janela.
        if clear {
            if self.filter.is_empty() {
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
            hint(ui, &["Ctrl+R"], "atualizar");
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
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = metrics::GAP;
                for &i in &visible {
                    let entry = &self.playlist[i];
                    let is_selected = self.selected.as_deref() == Some(entry.filename.as_str());
                    let action = self.row(ui, i, entry, is_selected);
                    if action.play {
                        play = Some(i);
                    }
                    if action.remove {
                        remove = Some(i);
                    }
                    if action.select || action.play {
                        select = Some(entry.filename.clone());
                    }
                }
                ui.add_space(metrics::GAP);
            });
        self.scroll_to_selected = false;
        if select.is_some() {
            self.selected = select;
        }
        if let Some(i) = play {
            self.play_or_pause(i);
        }
        if let Some(i) = remove {
            let r = self.mpv.remove(i);
            self.report(r);
        }
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(THEME_KEY, self.themes[self.theme].name.to_owned());
        storage.set_string(SHOW_HELP_KEY, self.show_help.to_string());
    }

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
