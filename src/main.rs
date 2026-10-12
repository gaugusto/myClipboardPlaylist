mod app;
mod cache;
mod cookies;
mod history;
mod metadata;
mod mpv;
mod single_instance;
mod theme;
mod url;

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
    /// Outra instância foi iniciada: trazer a janela para frente.
    Activate,
}

fn main() -> eframe::Result {
    let instance = match single_instance::acquire() {
        Ok(single_instance::Instance::Primary(guard)) => Some(guard),
        Ok(single_instance::Instance::Secondary) => {
            eprintln!("Clipboard Playlist já está aberto; trazendo a janela para frente.");
            return Ok(());
        }
        Err(e) => {
            eprintln!("aviso: não foi possível garantir instância única: {e:#}");
            None
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Clipboard Playlist")
            .with_app_id("myclipboardplaylist")
            .with_inner_size([680.0, 600.0])
            .with_min_inner_size([420.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        // Nome antigo mantido de propósito: o eframe guarda as configurações numa pasta com
        // esse nome, e trocá-lo perderia tema, cookies e ordem da fila já salvos.
        "myClipboardPlayList",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, instance)))),
    )
}
