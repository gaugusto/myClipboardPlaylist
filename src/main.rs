mod app;
mod cache;
mod cookies;
mod history;
mod metadata;
mod mpv;
mod settings;
mod single_instance;
mod ui;
mod url;

use std::sync::Arc;

use adw::prelude::*;

pub enum AppMsg {
    Mpv(mpv::MpvEvent),
    /// Links lidos do histórico do clipboard (ou o erro ao lê-lo), com a geração do
    /// histórico em que a leitura começou (ver `App::generation`).
    History(u64, Result<Vec<String>, String>),
    /// O conteúdo do clipboard mudou: reler o histórico.
    ClipboardChanged,
    /// Terminou uma exclusão no histórico do clipboard, com o aviso a mostrar (se houver).
    HistoryEdited(Result<Option<String>, String>),
    /// Metadados (yt-dlp) de uma URL.
    Metadata(String, Result<metadata::Metadata, String>),
    /// Thumbnail baixada da URL remota (quando não há cópia no cache), por URL do vídeo.
    Thumbnail(String, Result<Vec<u8>, String>),
    /// Outra instância foi iniciada: trazer a janela para frente.
    Activate,
}

/// Canal das threads de segundo plano para a interface. Mantém o backend independente do
/// toolkit: quem cria o `Notifier` decide como a mensagem chega à thread da interface.
#[derive(Clone)]
pub struct Notifier {
    send: Arc<dyn Fn(AppMsg) -> bool + Send + Sync>,
}

impl Notifier {
    /// `send` entrega a mensagem e devolve `false` se a interface já foi encerrada.
    pub fn new(send: impl Fn(AppMsg) -> bool + Send + Sync + 'static) -> Self {
        Self {
            send: Arc::new(send),
        }
    }

    /// Devolve `false` se a interface já foi encerrada.
    pub fn send(&self, msg: AppMsg) -> bool {
        (self.send)(msg)
    }
}

/// Id da janela no Wayland (`app-id`), usado por regras do compositor.
const APP_ID: &str = "myclipboardplaylist";

fn main() -> gtk::glib::ExitCode {
    let instance = match single_instance::acquire() {
        Ok(single_instance::Instance::Primary(guard)) => Some(guard),
        Ok(single_instance::Instance::Secondary) => {
            eprintln!("Clipboard Playlist já está aberto; trazendo a janela para frente.");
            return gtk::glib::ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("aviso: não foi possível garantir instância única: {e:#}");
            None
        }
    };
    // Sem `application_id`: a instância única é do `single_instance`, e o id da janela vem
    // do nome do programa, que mantém o `app-id` de antes.
    gtk::glib::set_prgname(Some(APP_ID));
    gtk::glib::set_application_name("Clipboard Playlist");
    let application = adw::Application::builder().build();
    let instance = std::cell::Cell::new(instance);
    application.connect_activate(move |application| {
        // `activate` só chega uma vez: sem `application_id`, não há outras instâncias.
        if let Some(window) = application.active_window() {
            window.present();
            return;
        }
        ui::build(application, instance.take());
    });
    application.run_with_args::<&str>(&[])
}
