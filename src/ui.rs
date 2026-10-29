//! Interface em GTK4/libadwaita. Os widgets são montados uma vez; a cada mudança no `App`
//! (`Shared::update`), `Shared::render` acerta os widgets com o estado. As cores vêm do tema
//! GTK do sistema (incluindo `~/.config/gtk-4.0/gtk.css`); `CSS` só usa as cores nomeadas
//! da libadwaita.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib, pango};

use crate::app::{App, Thumb};
use crate::cookies::Browser;
use crate::mpv::Entry;
use crate::settings::Settings;
use crate::single_instance::Guard;
use crate::{AppMsg, Notifier};

/// Tamanho da thumbnail e o menor, para janelas estreitas (abaixo de `NARROW_WINDOW`).
const THUMB_SIZE: (i32, i32) = (128, 72);
const THUMB_SIZE_SMALL: (i32, i32) = (96, 54);
const NARROW_WINDOW: i32 = 560;
const DEFAULT_SIZE: (i32, i32) = (680, 600);
const MIN_SIZE: (i32, i32) = (420, 320);
const PLAY_BUTTON: i32 = 38;
/// Distância da borda da lista em que arrastar um item a faz rolar, e quanto ela rola a
/// cada passo (`AUTOSCROLL_STEP`).
const DRAG_SCROLL_EDGE: f64 = 48.0;
const DRAG_SCROLL_SPEED: f64 = 10.0;
const AUTOSCROLL_STEP: Duration = Duration::from_millis(16);
/// Tempo com o botão pressionado sobre um item até ele ficar semitransparente, indicando
/// que pode ser arrastado. Mais curto que isso é um clique, que não muda a aparência.
const HOLD_DELAY: Duration = Duration::from_millis(200);
/// Opacidade da cópia do cartão que acompanha o ponteiro ao arrastar.
const DRAG_ICON_OPACITY: f64 = 0.6;

const CSS: &str = r#"
.queue { background: none; }
.queue > row { padding: 0; margin-bottom: 8px; background: none; border-radius: 12px; }
.queue > row:hover, .queue > row:selected, .queue > row:focus { background: none; }
.queue-card {
  padding: 10px;
  border-radius: 12px;
  border: 1px solid alpha(currentColor, 0.1);
  background-color: @card_bg_color;
  color: @card_fg_color;
}
.queue > row:hover .queue-card { background-color: mix(@card_bg_color, @card_fg_color, 0.06); }
.queue-card.current {
  background-color: mix(@card_bg_color, @accent_bg_color, 0.14);
  border-color: alpha(@accent_bg_color, 0.5);
}
.queue > row:selected .queue-card {
  border-color: @accent_bg_color;
  box-shadow: inset 0 0 0 1px @accent_bg_color;
}
.position { font-size: 18px; font-feature-settings: "tnum"; opacity: 0.55; }
.current .position { color: @accent_bg_color; opacity: 1; }
.thumb { border-radius: 6px; background-color: alpha(currentColor, 0.08); }
.badge {
  margin: 4px;
  padding: 1px 5px;
  border-radius: 4px;
  font-size: 11px;
  color: white;
  background-color: rgba(0, 0, 0, 0.75);
}
.entry-title { font-size: 15px; font-weight: bold; }
.details { font-size: 12.5px; }
.pill {
  padding: 1px 6px;
  border-radius: 6px;
  font-size: 10.5px;
  font-weight: bold;
  color: @accent_bg_color;
  background-color: alpha(@accent_bg_color, 0.18);
}
.play { min-width: 38px; min-height: 38px; padding: 0; }
.remove { opacity: 0.5; }
.queue > row:hover .remove, .queue > row:selected .remove, .current .remove { opacity: 1; }
.remove:hover { color: @destructive_bg_color; background-color: alpha(@destructive_bg_color, 0.15); }
.status { padding: 8px 12px; border-radius: 8px; font-size: 12.5px; background-color: @card_bg_color; }
.status.error { color: @error_bg_color; background-color: alpha(@error_bg_color, 0.12); }
.keycap {
  padding: 1px 5px;
  border-radius: 4px;
  border: 1px solid alpha(currentColor, 0.2);
  font-family: monospace;
  font-size: 11px;
  background-color: @card_bg_color;
}
.hint { font-size: 11.5px; }
.queue > row.dragging { opacity: 0.45; }
.queue > row.drop-above { box-shadow: 0 -4px 0 -1px @accent_bg_color; }
.queue > row.drop-below { box-shadow: 0 4px 0 -1px @accent_bg_color; }
"#;

/// Widgets de um item da fila.
struct Row {
    row: gtk::ListBoxRow,
    card: gtk::Box,
    position: gtk::Label,
    thumb_clamps: (adw::Clamp, adw::Clamp),
    thumb_stack: gtk::Stack,
    picture: gtk::Picture,
    badge: gtk::Label,
    title: gtk::Label,
    pill: gtk::Label,
    details: gtk::Label,
    play: gtk::Button,
    play_stack: gtk::Stack,
    play_icon: gtk::Image,
}

/// Thumbnail decodificada, com a origem de onde veio (para recarregar se ela mudar).
struct Texture {
    source: String,
    texture: Option<gdk::Texture>,
}

struct Widgets {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    browser: gtk::DropDown,
    clear: gtk::Button,
    search: gtk::SearchEntry,
    status: gtk::Box,
    status_icon: gtk::Image,
    status_label: gtk::Label,
    stack: gtk::Stack,
    no_matches: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    list: gtk::ListBox,
    toolbar: adw::ToolbarView,
}

struct Shared {
    /// O próprio `Rc`, para os sinais das linhas criadas em `render`.
    this: Weak<Shared>,
    app: RefCell<App>,
    widgets: Widgets,
    rows: RefCell<HashMap<u64, Row>>,
    /// Ids dos itens na ordem em que estão na `ListBox`.
    order: RefCell<Vec<u64>>,
    textures: RefCell<HashMap<String, Texture>>,
    /// `render` está mexendo nos widgets: os sinais que isso dispara são ignorados.
    syncing: Cell<bool>,
    /// Próximo prazo do `App` (confirmação do Limpar, bloqueio de abertura).
    timer: RefCell<Option<glib::SourceId>>,
    /// Últimas configurações gravadas, para só gravar quando mudarem.
    saved: RefCell<Settings>,
    /// Mantém o socket de instância única enquanto o app estiver aberto.
    instance: RefCell<Option<Guard>>,
    /// Arrastar um item: a linha marcada com a posição de destino (e a classe usada), a
    /// altura do ponteiro na área visível da lista e a rolagem automática perto das bordas.
    drop_mark: RefCell<Option<(gtk::ListBoxRow, &'static str)>>,
    drag_y: Cell<f64>,
    autoscroll: RefCell<Option<glib::SourceId>>,
}

/// Monta a janela e liga as mensagens das threads de segundo plano à interface.
pub fn build(application: &adw::Application, mut instance: Option<Guard>) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let (tx, rx) = async_channel::unbounded();
    let notifier = Notifier::new(move |msg| tx.send_blocking(msg).is_ok());
    if let Some(guard) = &mut instance {
        guard.listen(notifier.clone());
    }
    let settings = Settings::load();
    let app = App::new(notifier, &settings);
    let widgets = build_widgets(application, &settings);
    let shared = Rc::new_cyclic(|this| Shared {
        this: this.clone(),
        app: RefCell::new(app),
        widgets,
        rows: RefCell::new(HashMap::new()),
        order: RefCell::new(Vec::new()),
        textures: RefCell::new(HashMap::new()),
        syncing: Cell::new(false),
        timer: RefCell::new(None),
        saved: RefCell::new(settings),
        instance: RefCell::new(instance),
        drop_mark: RefCell::new(None),
        drag_y: Cell::new(0.0),
        autoscroll: RefCell::new(None),
    });
    connect(&shared);
    glib::spawn_future_local({
        let shared = shared.clone();
        async move {
            while let Ok(msg) = rx.recv().await {
                let mut batch = vec![msg];
                batch.extend(std::iter::from_fn(|| rx.try_recv().ok()));
                if batch.iter().any(|m| matches!(m, AppMsg::Activate)) {
                    shared.widgets.window.present();
                }
                shared.update(|app| {
                    for msg in batch {
                        app.handle_message(msg);
                    }
                });
            }
        }
    });
    shared.update(|_| {});
    shared.widgets.window.present();
    shared.widgets.search.grab_focus();
}

fn build_widgets(application: &adw::Application, settings: &Settings) -> Widgets {
    let (width, height) = settings.window_size.unwrap_or(DEFAULT_SIZE);
    let title = adw::WindowTitle::new("Clipboard Playlist", "");

    let names: Vec<&str> = std::iter::once("nenhum")
        .chain(Browser::ALL.iter().map(|b| b.name()))
        .collect();
    let browser = gtk::DropDown::from_strings(&names);
    browser.set_tooltip_text(Some(
        "Navegador de onde o yt-dlp lê os cookies, para vídeos que pedem login \
         (\"Sign in to confirm you're not a bot\").",
    ));
    let cookies = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let cookies_label = gtk::Label::new(Some("Cookies"));
    cookies_label.add_css_class("dim-label");
    cookies.append(&cookies_label);
    cookies.append(&browser);

    let clear = gtk::Button::with_label("Limpar");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_start(&cookies);
    header.pack_end(&clear);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filtrar por título ou site…"));
    search.set_search_delay(0);
    let status_icon = gtk::Image::new();
    let status_label = gtk::Label::new(None);
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);
    status_label.set_ellipsize(pango::EllipsizeMode::End);
    let status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status.add_css_class("status");
    status.append(&status_icon);
    status.append(&status_label);
    let top = gtk::Box::new(gtk::Orientation::Vertical, 12);
    top.set_margin_start(16);
    top.set_margin_end(16);
    top.set_margin_top(6);
    top.set_margin_bottom(12);
    top.append(&search);
    top.append(&status);

    let empty = adw::StatusPage::builder()
        .icon_name("edit-paste-symbolic")
        .title("Nenhum vídeo na fila")
        .description("Copie links de vídeos: eles entram na fila sozinhos.")
        .build();
    let no_matches = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .description("Esc limpa o filtro.")
        .build();
    let list = gtk::ListBox::new();
    list.add_css_class("queue");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.set_valign(gtk::Align::Start);
    list.set_margin_start(16);
    list.set_margin_end(16);
    list.set_margin_bottom(8);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .vexpand(true)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&no_matches, Some("no-matches"));
    stack.add_named(&scroller, Some("list"));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&top);
    toolbar.set_content(Some(&stack));
    toolbar.add_bottom_bar(&footer());

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Clipboard Playlist")
        .default_width(width)
        .default_height(height)
        .content(&toolbar)
        .build();
    window.set_size_request(MIN_SIZE.0, MIN_SIZE.1);
    search.set_key_capture_widget(Some(&window));

    Widgets {
        window,
        title,
        browser,
        clear,
        search,
        status,
        status_icon,
        status_label,
        stack,
        no_matches,
        scroller,
        list,
        toolbar,
    }
}

/// Rodapé com a ajuda de atalhos.
fn footer() -> adw::WrapBox {
    let wrap = adw::WrapBox::new();
    wrap.set_child_spacing(14);
    wrap.set_line_spacing(6);
    wrap.set_margin_start(16);
    wrap.set_margin_end(16);
    wrap.set_margin_top(8);
    wrap.set_margin_bottom(8);
    let hints: [(&[&str], &str); 6] = [
        (&["↑", "↓", "Ctrl+J", "Ctrl+K"], "navegar"),
        (&["Enter"], "tocar/pausar"),
        (&["Alt+↑", "Alt+↓"], "mover na fila"),
        (&["Ctrl+L"], "limpar tudo"),
        (&["Esc"], "limpar filtro / fechar"),
        (&["Ctrl+H"], "ocultar ajuda"),
    ];
    for (keys, text) in hints {
        let hint = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        for key in keys {
            let cap = gtk::Label::new(Some(key));
            cap.add_css_class("keycap");
            hint.append(&cap);
        }
        let label = gtk::Label::new(Some(text));
        label.add_css_class("hint");
        label.add_css_class("dim-label");
        label.set_margin_start(3);
        hint.append(&label);
        wrap.append(&hint);
    }
    wrap
}

fn connect(shared: &Rc<Shared>) {
    let w = &shared.widgets;
    w.clear.connect_clicked({
        let shared = shared.clone();
        move |_| shared.update(App::clear_command)
    });
    w.browser.connect_selected_notify({
        let shared = shared.clone();
        move |dropdown| {
            let browser = (dropdown.selected() as usize)
                .checked_sub(1)
                .and_then(|i| Browser::ALL.get(i).copied());
            shared.react(|app| app.choose_browser(browser));
        }
    });
    w.search.connect_search_changed({
        let shared = shared.clone();
        move |search| {
            let text = search.text();
            shared.react(|app| app.set_filter(&text));
        }
    });
    w.list.connect_row_selected({
        let shared = shared.clone();
        // `row_id` só dentro do `react`: durante o `render`, `rows` está emprestado.
        move |_, row| {
            shared.react(|app| {
                if let Some(id) = row.and_then(|row| shared.row_id(row)) {
                    app.select(id);
                }
            });
        }
    });
    w.list.connect_row_activated({
        let shared = shared.clone();
        move |_, row| {
            shared.react(|app| {
                if let Some(id) = shared.row_id(row) {
                    app.play_or_pause(id);
                }
            });
        }
    });
    // Ao voltar para a janela, relê o histórico: ele pode ter sido editado fora do app.
    w.window.connect_is_active_notify({
        let shared = shared.clone();
        move |window| {
            if window.is_active() {
                shared.react(App::refresh);
            }
        }
    });
    // A largura decide o tamanho das thumbnails.
    w.window.connect_default_width_notify({
        let shared = shared.clone();
        move |_| shared.react(|_| {})
    });
    w.window.connect_close_request({
        let shared = shared.clone();
        move |_| {
            shared.save();
            shared.app.borrow_mut().shutdown();
            shared.instance.borrow_mut().take();
            glib::Propagation::Proceed
        }
    });
    let drop = gtk::DropTarget::new(u64::static_type(), gdk::DragAction::MOVE);
    drop.connect_motion({
        let shared = shared.clone();
        move |_, _, y| {
            shared.drag_motion(y);
            gdk::DragAction::MOVE
        }
    });
    drop.connect_leave({
        let shared = shared.clone();
        move |_| shared.drag_done()
    });
    drop.connect_drop({
        let shared = shared.clone();
        move |_, value, _, y| {
            let slot = shared.drop_slot(y);
            shared.drag_done();
            let (Ok(id), Some(slot)) = (value.get::<u64>(), slot) else {
                return false;
            };
            shared.update(|app| app.drop_at(id, slot));
            true
        }
    });
    w.scroller.add_controller(drop);
    let keys = gtk::EventControllerKey::new();
    // Antes do campo de filtro (sempre em foco), que ficaria com as setas, o Enter e o Esc.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let shared = shared.clone();
        move |_, key, _, state| shared.key_pressed(key, state)
    });
    w.window.add_controller(keys);
}

impl Shared {
    /// Aplica `f` ao estado e acerta a interface.
    fn update(&self, f: impl FnOnce(&mut App)) {
        let scroll = {
            let mut app = self.app.borrow_mut();
            f(&mut app);
            app.keep_selection();
            std::mem::take(&mut app.scroll_to_selected)
        };
        self.render();
        if scroll {
            self.scroll_to_selected();
        }
        self.schedule_expiry();
        self.save_if_changed();
    }

    /// `update` vindo de um sinal de widget: ignorado se o próprio `render` o disparou.
    fn react(&self, f: impl FnOnce(&mut App)) {
        if !self.syncing.get() {
            self.update(f);
        }
    }

    fn window_size(&self) -> Option<(i32, i32)> {
        let window = &self.widgets.window;
        let (width, height) = window.default_size();
        (width > 0 && height > 0 && !window.is_maximized() && !window.is_fullscreen())
            .then_some((width, height))
            .or(self.saved.borrow().window_size)
    }

    fn save(&self) {
        let settings = self.app.borrow().settings(self.window_size());
        if let Err(e) = settings.save() {
            eprintln!("aviso: {e:#}");
        }
        *self.saved.borrow_mut() = settings;
    }

    /// Grava as configurações se mudaram (a ordem da fila muda a cada evento do mpv). O
    /// tamanho da janela só é gravado ao fechar.
    fn save_if_changed(&self) {
        let window_size = self.saved.borrow().window_size;
        let settings = self.app.borrow().settings(window_size);
        if settings != *self.saved.borrow() {
            self.save();
        }
    }

    /// Agenda um `update` para quando vencer o próximo prazo do `App`.
    fn schedule_expiry(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let Some(left) = self.app.borrow_mut().expire() else {
            return;
        };
        let shared = self.weak();
        let timer = glib::timeout_add_local_once(left + Duration::from_millis(20), move || {
            if let Some(shared) = shared.upgrade() {
                shared.timer.borrow_mut().take();
                shared.update(|_| {});
            }
        });
        *self.timer.borrow_mut() = Some(timer);
    }

    fn weak(&self) -> Weak<Shared> {
        self.this.clone()
    }

    fn row_id(&self, row: &gtk::ListBoxRow) -> Option<u64> {
        self.rows
            .borrow()
            .iter()
            .find(|(_, r)| r.row == *row)
            .map(|(&id, _)| id)
    }

    /// Atalhos de teclado. As setas, o Enter e o Esc só valem com o foco no filtro ou na
    /// lista, para não atrapalhar o seletor de cookies e os botões.
    fn key_pressed(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let plain = !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SHIFT_MASK
                | gdk::ModifierType::SUPER_MASK,
        );
        let ours = plain && self.owns_keys();
        let alt = state.contains(gdk::ModifierType::ALT_MASK)
            && !state.intersects(
                gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::SHIFT_MASK
                    | gdk::ModifierType::SUPER_MASK,
            )
            && self.owns_keys();
        let key = key.to_lower();
        match key {
            gdk::Key::Down if alt => self.update(|app| app.move_selected(1)),
            gdk::Key::Up if alt => self.update(|app| app.move_selected(-1)),
            gdk::Key::Down if ours => self.update(|app| app.move_selection(1)),
            gdk::Key::j if ctrl => self.update(|app| app.move_selection(1)),
            gdk::Key::Up if ours => self.update(|app| app.move_selection(-1)),
            gdk::Key::k if ctrl => self.update(|app| app.move_selection(-1)),
            gdk::Key::Return | gdk::Key::KP_Enter if ours => self.update(App::play_selected),
            gdk::Key::l if ctrl => self.update(App::clear_command),
            gdk::Key::h if ctrl => self.update(|app| app.show_help = !app.show_help),
            // Esc cancela o Limpar e limpa o filtro; com o filtro já vazio, fecha a janela.
            gdk::Key::Escape if ours => {
                if self.app.borrow_mut().escape() {
                    self.update(|_| {});
                } else {
                    self.widgets.window.close();
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// O foco está no campo de filtro ou na lista (ou em lugar nenhum).
    fn owns_keys(&self) -> bool {
        let w = &self.widgets;
        GtkWindowExt::focus(&w.window).is_none_or(|focus| {
            focus.is_ancestor(&w.search)
                || focus == *w.search.upcast_ref::<gtk::Widget>()
                || focus.is_ancestor(&w.list)
                || focus == *w.list.upcast_ref::<gtk::Widget>()
        })
    }

    fn render(&self) {
        self.syncing.set(true);
        let app = self.app.borrow();
        let w = &self.widgets;

        w.title.set_subtitle(&format!(
            "{}  ·  fonte: {}",
            app.count_text(),
            app.source_name()
        ));
        let armed = app.clear_armed();
        w.clear
            .set_label(if armed { "Confirmar limpeza" } else { "Limpar" });
        if armed {
            w.clear.add_css_class("destructive-action");
        } else {
            w.clear.remove_css_class("destructive-action");
        }
        w.clear.set_tooltip_text(Some(&format!(
            "Exclui todos os vídeos da fila, apaga o histórico do clipboard ({}) e o cache. \
             Clique de novo para confirmar. Atalho: Ctrl+L",
            app.source_name()
        )));
        let browser = app
            .browser
            .and_then(|b| Browser::ALL.iter().position(|&a| a == b))
            .map_or(0, |i| i + 1) as u32;
        if w.browser.selected() != browser {
            w.browser.set_selected(browser);
        }
        if w.search.text() != app.filter {
            w.search.set_text(&app.filter);
        }
        match &app.status {
            Some(status) => {
                w.status.set_visible(true);
                w.status_label.set_text(&status.text);
                w.status_label.set_tooltip_text(Some(&status.text));
                w.status_icon.set_icon_name(Some(if status.is_error {
                    "dialog-warning-symbolic"
                } else {
                    "dialog-information-symbolic"
                }));
                if status.is_error {
                    w.status.add_css_class("error");
                } else {
                    w.status.remove_css_class("error");
                }
            }
            None => w.status.set_visible(false),
        }
        w.toolbar.set_reveal_bottom_bars(app.show_help);

        let visible = app.visible();
        if app.playlist.is_empty() {
            w.stack.set_visible_child_name("empty");
        } else if visible.is_empty() {
            w.no_matches.set_title(&format!(
                "Nenhum vídeo corresponde a “{}”",
                app.filter.trim()
            ));
            w.stack.set_visible_child_name("no-matches");
        } else {
            w.stack.set_visible_child_name("list");
        }
        self.render_rows(&app, &visible);
        self.textures
            .borrow_mut()
            .retain(|url, _| app.playlist.iter().any(|e| e.filename == *url));
        drop(app);
        self.syncing.set(false);
    }

    /// Acerta as linhas da lista com a fila: cria as novas, tira as que saíram, mantém a
    /// ordem do mpv e atualiza o conteúdo de cada uma.
    fn render_rows(&self, app: &App, visible: &[usize]) {
        let list = &self.widgets.list;
        let mut rows = self.rows.borrow_mut();
        let ids: Vec<u64> = app.playlist.iter().map(|e| e.id).collect();
        rows.retain(|id, row| {
            let keep = ids.contains(id);
            if !keep {
                list.remove(&row.row);
            }
            keep
        });
        for entry in &app.playlist {
            rows.entry(entry.id)
                .or_insert_with(|| self.new_row(entry.id));
        }
        let mut order = self.order.borrow_mut();
        // Só as linhas fora do lugar são reinseridas: num arraste, só a que foi movida.
        if *order != ids {
            for (i, id) in ids.iter().enumerate() {
                let row = &rows[id].row;
                if list.row_at_index(i as i32).as_ref() != Some(row) {
                    if row.parent().is_some() {
                        list.remove(row);
                    }
                    list.insert(row, i as i32);
                }
            }
            *order = ids;
        }

        let width = self.widgets.window.width();
        let width = if width > 0 {
            width
        } else {
            self.widgets.window.default_width()
        };
        let thumb = if width < NARROW_WINDOW {
            THUMB_SIZE_SMALL
        } else {
            THUMB_SIZE
        };
        let digits = app.playlist.len().max(1).to_string().len() as i32;
        let mut selected = None;
        for (index, entry) in app.playlist.iter().enumerate() {
            let row = &rows[&entry.id];
            row.row.set_visible(visible.contains(&index));
            if row.row.is_visible()
                && selected.is_none()
                && app.selected.as_deref() == Some(entry.filename.as_str())
            {
                selected = Some(row.row.clone());
            }
            self.render_row(app, row, index, entry, thumb, digits);
        }
        if list.selected_row() != selected {
            match &selected {
                Some(row) => list.select_row(Some(row)),
                None => list.unselect_all(),
            }
        }
    }

    fn render_row(
        &self,
        app: &App,
        row: &Row,
        index: usize,
        entry: &Entry,
        thumb: (i32, i32),
        digits: i32,
    ) {
        let title = app.entry_title(entry);
        if entry.current {
            row.card.add_css_class("current");
        } else {
            row.card.remove_css_class("current");
        }
        row.position.set_text(&(index + 1).to_string());
        row.position.set_width_chars(digits);
        row.title.set_text(&title);
        row.pill.set_visible(entry.current);
        row.pill.set_text(if app.is_paused() {
            "PAUSADO"
        } else {
            "TOCANDO"
        });
        row.details.set_text(&app.entry_details(entry));

        let playing = app.is_playing(entry);
        let mut tooltip = format!(
            "{title}\n{}\nClique duplo para {}; arraste para mudar a posição na fila",
            entry.filename,
            if playing { "pausar" } else { "tocar" }
        );
        if let Some(e) = app.entry_error(entry) {
            tooltip.push_str(&format!("\n\nyt-dlp: {e}"));
        }
        row.card.set_tooltip_text(Some(&tooltip));

        let (clamp_w, clamp_h) = &row.thumb_clamps;
        clamp_w.set_maximum_size(thumb.0);
        clamp_w.set_tightening_threshold(thumb.0);
        clamp_h.set_maximum_size(thumb.1);
        clamp_h.set_tightening_threshold(thumb.1);
        row.thumb_stack.set_size_request(thumb.0, thumb.1);
        match self.texture(&entry.filename, app.thumb(&entry.filename)) {
            Ok(texture) => {
                row.picture.set_paintable(Some(&texture));
                row.thumb_stack.set_visible_child_name("image");
            }
            Err(true) => row.thumb_stack.set_visible_child_name("loading"),
            Err(false) => row.thumb_stack.set_visible_child_name("none"),
        }
        let badge = app.duration_badge(entry);
        row.badge.set_visible(badge.is_some());
        row.badge.set_text(badge.as_deref().unwrap_or_default());

        // Enquanto um vídeo abre, os botões ficam apagados e o do vídeo que abre gira.
        let opening = app.opening();
        row.play
            .set_sensitive(opening.is_none_or(|id| id == entry.id));
        row.play.set_can_target(opening.is_none());
        if opening == Some(entry.id) {
            row.play_stack.set_visible_child_name("spinner");
            row.play.set_tooltip_text(None);
        } else {
            row.play_stack.set_visible_child_name("icon");
            row.play_icon.set_icon_name(Some(if playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            }));
            row.play.set_tooltip_text(Some(if playing {
                "Pausar"
            } else if app.is_active(entry) {
                "Continuar"
            } else {
                "Tocar"
            }));
        }
    }

    /// Textura da thumbnail de `url`. Sem imagem, `Err(true)` se ela ainda está a caminho.
    fn texture(&self, url: &str, thumb: Thumb<'_>) -> Result<gdk::Texture, bool> {
        let (source, bytes) = match thumb {
            Thumb::File(path) => (path.display().to_string(), None),
            Thumb::Bytes(bytes) => (format!("remote:{:p}", bytes.as_ptr()), Some(bytes)),
            Thumb::Loading => return Err(true),
            Thumb::None => return Err(false),
        };
        let mut textures = self.textures.borrow_mut();
        if let Some(cached) = textures.get(url).filter(|t| t.source == source) {
            return cached.texture.clone().ok_or(false);
        }
        let texture = match (bytes, &thumb) {
            (Some(bytes), _) => decode(bytes),
            (None, Thumb::File(path)) => std::fs::read(path).ok().and_then(|b| decode(&b)),
            _ => None,
        };
        textures.insert(
            url.to_owned(),
            Texture {
                source,
                texture: texture.clone(),
            },
        );
        texture.ok_or(false)
    }

    fn new_row(&self, id: u64) -> Row {
        let position = gtk::Label::new(None);
        position.add_css_class("position");
        position.set_xalign(1.0);

        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        let loading = adw::Spinner::new();
        let none = gtk::Image::from_icon_name("video-x-generic-symbolic");
        none.set_pixel_size(24);
        none.add_css_class("dim-label");
        let thumb_stack = gtk::Stack::new();
        thumb_stack.add_css_class("thumb");
        thumb_stack.set_overflow(gtk::Overflow::Hidden);
        thumb_stack.add_named(&picture, Some("image"));
        thumb_stack.add_named(&loading, Some("loading"));
        thumb_stack.add_named(&none, Some("none"));
        // Duas `Clamp` limitam a imagem ao tamanho da thumbnail (a `Picture` pediria o
        // tamanho natural da imagem).
        let clamp_h = adw::Clamp::builder()
            .orientation(gtk::Orientation::Vertical)
            .child(&thumb_stack)
            .build();
        let clamp_w = adw::Clamp::builder().child(&clamp_h).build();
        let badge = gtk::Label::new(None);
        badge.add_css_class("badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::End);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&clamp_w));
        overlay.add_overlay(&badge);
        overlay.set_valign(gtk::Align::Center);

        let title = gtk::Label::new(None);
        title.add_css_class("entry-title");
        title.set_xalign(0.0);
        title.set_ellipsize(pango::EllipsizeMode::End);
        let pill = gtk::Label::new(None);
        pill.add_css_class("pill");
        pill.set_valign(gtk::Align::Center);
        let details = gtk::Label::new(None);
        details.add_css_class("details");
        details.add_css_class("dim-label");
        details.set_xalign(0.0);
        details.set_ellipsize(pango::EllipsizeMode::End);
        let info_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        info_line.append(&pill);
        info_line.append(&details);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&title);
        text.append(&info_line);

        let play_icon = gtk::Image::new();
        let play_stack = gtk::Stack::new();
        play_stack.add_named(&play_icon, Some("icon"));
        play_stack.add_named(&adw::Spinner::new(), Some("spinner"));
        let play = gtk::Button::builder()
            .child(&play_stack)
            .valign(gtk::Align::Center)
            .width_request(PLAY_BUTTON)
            .height_request(PLAY_BUTTON)
            .css_classes(["circular", "suggested-action", "play"])
            .build();
        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Remover da fila e do histórico do clipboard")
            .css_classes(["flat", "circular", "remove"])
            .build();
        let shared = self.weak();
        play.connect_clicked({
            let shared = shared.clone();
            move |_| {
                if let Some(shared) = shared.upgrade() {
                    shared.update(|app| {
                        app.select(id);
                        app.play_or_pause(id);
                    });
                }
            }
        });
        remove.connect_clicked({
            let shared = shared.clone();
            move |_| {
                if let Some(shared) = shared.upgrade() {
                    shared.update(|app| app.remove_entry(id));
                }
            }
        });

        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("queue-card");
        card.append(&position);
        card.append(&overlay);
        card.append(&text);
        card.append(&play);
        card.append(&remove);
        let row = gtk::ListBoxRow::builder().child(&card).build();
        let dragging = Rc::new(Cell::new(false));
        row.add_controller(hold_feedback(&row, dragging.clone()));
        row.add_controller(drag_source(id, &row, dragging, shared));
        Row {
            row,
            card,
            position,
            thumb_clamps: (clamp_w, clamp_h),
            thumb_stack,
            picture,
            badge,
            title,
            pill,
            details,
            play,
            play_stack,
            play_icon,
        }
    }

    /// Posição de destino de um item solto à altura `y` da área visível da lista: quantas
    /// linhas visíveis ficam acima do ponteiro (comparando com o meio de cada uma), e as
    /// linhas visíveis, na ordem da tela.
    fn drop_slot_rows(&self, y: f64) -> (Option<usize>, Vec<gtk::ListBoxRow>) {
        let w = &self.widgets;
        let rows = self.rows.borrow();
        let visible: Vec<gtk::ListBoxRow> = self
            .order
            .borrow()
            .iter()
            .filter_map(|id| rows.get(id))
            .map(|r| r.row.clone())
            .filter(|row| row.is_visible())
            .collect();
        let point = gtk::graphene::Point::new(0.0, y as f32);
        let Some(y) = w.scroller.compute_point(&w.list, &point).map(|p| p.y()) else {
            return (None, visible);
        };
        let slot = visible
            .iter()
            .filter(|row| {
                row.compute_bounds(&w.list)
                    .is_some_and(|b| b.y() + b.height() / 2.0 < y)
            })
            .count();
        (Some(slot), visible)
    }

    fn drop_slot(&self, y: f64) -> Option<usize> {
        self.drop_slot_rows(y).0
    }

    /// O ponteiro se moveu durante um arraste: marca onde o item vai cair e liga ou
    /// desliga a rolagem automática perto das bordas.
    fn drag_motion(&self, y: f64) {
        self.drag_y.set(y);
        let (slot, visible) = self.drop_slot_rows(y);
        let mark = slot.and_then(|slot| match visible.get(slot) {
            Some(row) => Some((row.clone(), "drop-above")),
            None => visible.last().map(|row| (row.clone(), "drop-below")),
        });
        let mut current = self.drop_mark.borrow_mut();
        if *current != mark {
            if let Some((row, class)) = current.take() {
                row.remove_css_class(class);
            }
            if let Some((row, class)) = &mark {
                row.add_css_class(class);
            }
            *current = mark;
        }
        drop(current);

        let height = f64::from(self.widgets.scroller.height());
        let near_edge = y < DRAG_SCROLL_EDGE || y > height - DRAG_SCROLL_EDGE;
        let mut autoscroll = self.autoscroll.borrow_mut();
        if near_edge && autoscroll.is_none() {
            let shared = self.weak();
            *autoscroll = Some(glib::timeout_add_local(AUTOSCROLL_STEP, move || {
                let Some(shared) = shared.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let y = shared.drag_y.get();
                let height = f64::from(shared.widgets.scroller.height());
                let speed = if y < DRAG_SCROLL_EDGE {
                    -DRAG_SCROLL_SPEED
                } else if y > height - DRAG_SCROLL_EDGE {
                    DRAG_SCROLL_SPEED
                } else {
                    shared.autoscroll.borrow_mut().take();
                    return glib::ControlFlow::Break;
                };
                let adj = shared.widgets.scroller.vadjustment();
                adj.set_value(adj.value() + speed);
                // A lista andou sob o ponteiro parado: acerta a marca de destino.
                shared.drag_motion(y);
                glib::ControlFlow::Continue
            }));
        } else if !near_edge && let Some(timer) = autoscroll.take() {
            timer.remove();
        }
    }

    /// Fim de um arraste (soltou, cancelou ou saiu da lista): tira a marca e para a rolagem.
    fn drag_done(&self) {
        if let Some((row, class)) = self.drop_mark.borrow_mut().take() {
            row.remove_css_class(class);
        }
        if let Some(timer) = self.autoscroll.borrow_mut().take() {
            timer.remove();
        }
    }

    /// Rola a lista até o item selecionado, depois que a lista for redimensionada.
    fn scroll_to_selected(&self) {
        let shared = self.weak();
        glib::idle_add_local_once(move || {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let w = &shared.widgets;
            let Some(row) = w.list.selected_row() else {
                return;
            };
            let Some(bounds) = row.compute_bounds(&w.list) else {
                return;
            };
            let adj = w.scroller.vadjustment();
            let (top, bottom) = (bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
            if top < adj.value() {
                adj.set_value(top);
            } else if bottom > adj.value() + adj.page_size() {
                adj.set_value(bottom - adj.page_size());
            }
        });
    }
}

/// Começa o arraste de uma linha: leva o id do item, mostra o cartão sob o ponteiro e
/// apaga a linha enquanto ela é arrastada.
fn drag_source(
    id: u64,
    row: &gtk::ListBoxRow,
    dragging: Rc<Cell<bool>>,
    shared: Weak<Shared>,
) -> gtk::DragSource {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    let hotspot = Rc::new(Cell::new((0.0, 0.0)));
    source.connect_prepare({
        let hotspot = hotspot.clone();
        move |_, x, y| {
            hotspot.set((x, y));
            Some(gdk::ContentProvider::for_value(&id.to_value()))
        }
    });
    source.connect_drag_begin({
        let (row, dragging) = (row.clone(), dragging.clone());
        move |_, drag| {
            let (x, y) = hotspot.get();
            // Uma imagem do cartão, semitransparente, acompanha o ponteiro.
            let picture =
                gtk::Picture::for_paintable(&gtk::WidgetPaintable::new(row.child().as_ref()));
            picture.set_can_shrink(false);
            picture.set_opacity(DRAG_ICON_OPACITY);
            if let Ok(icon) = gtk::DragIcon::for_drag(drag).downcast::<gtk::DragIcon>() {
                icon.set_child(Some(&picture));
            }
            drag.set_hotspot(x as i32, y as i32);
            dragging.set(true);
            row.add_css_class("dragging");
        }
    });
    source.connect_drag_end({
        let row = row.clone();
        move |_, _, _| {
            dragging.set(false);
            row.remove_css_class("dragging");
            if let Some(shared) = shared.upgrade() {
                shared.drag_done();
            }
        }
    });
    source
}

/// Deixa a linha semitransparente quando o botão fica pressionado sobre ela por
/// `HOLD_DELAY`, antes de o ponteiro se mover. Ao soltar sem arrastar, ela volta ao normal;
/// se o arraste começar, `drag_source` cuida dela (`dragging`).
fn hold_feedback(row: &gtk::ListBoxRow, dragging: Rc<Cell<bool>>) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    gesture.connect_pressed({
        let (row, timer) = (row.clone(), timer.clone());
        move |_, _, _, _| {
            if let Some(old) = timer.borrow_mut().take() {
                old.remove();
            }
            let (row, pending) = (row.clone(), timer.clone());
            let id = glib::timeout_add_local_once(HOLD_DELAY, move || {
                pending.borrow_mut().take();
                row.add_css_class("dragging");
            });
            *timer.borrow_mut() = Some(id);
        }
    });
    // Soltou (`released`) ou o gesto acabou sem clique (`stopped`, por exemplo quando o
    // arraste assume o ponteiro).
    let release = {
        let row = row.clone();
        move || {
            if let Some(pending) = timer.borrow_mut().take() {
                pending.remove();
            }
            if !dragging.get() {
                row.remove_css_class("dragging");
            }
        }
    };
    let release = Rc::new(release);
    gesture.connect_released({
        let release = release.clone();
        move |_, _, _, _| release()
    });
    gesture.connect_stopped(move |_| release());
    gesture
}

/// Decodifica uma imagem: primeiro pelo GTK (PNG, JPEG), depois pelo `image` (WebP).
fn decode(bytes: &[u8]) -> Option<gdk::Texture> {
    if let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from(bytes)) {
        return Some(texture);
    }
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    let pixels = glib::Bytes::from_owned(image.into_raw());
    Some(
        gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &pixels,
            width as usize * 4,
        )
        .upcast(),
    )
}
