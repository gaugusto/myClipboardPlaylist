mod app;
mod history;
mod metadata;
mod mpv;
mod single_instance;
mod theme;
mod url;

pub enum AppMsg {
    Mpv(mpv::MpvEvent),
    /// Links lidos do histórico do clipboard (ou o erro ao lê-lo).
    History(Result<Vec<String>, String>),
    /// Metadados (yt-dlp) de uma URL.
    Metadata(String, Result<metadata::Metadata, String>),
    /// Outra instância foi iniciada: trazer a janela para frente.
    Activate,
}

fn main() -> eframe::Result {
    let instance = match single_instance::acquire() {
        Ok(single_instance::Instance::Primary(guard)) => Some(guard),
        Ok(single_instance::Instance::Secondary) => {
            eprintln!("myClipboardPlayList já está aberto; trazendo a janela para frente.");
            return Ok(());
        }
        Err(e) => {
            eprintln!("aviso: não foi possível garantir instância única: {e:#}");
            None
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("myClipboardPlayList")
            .with_app_id("myclipboardplaylist")
            .with_inner_size([680.0, 600.0])
            .with_min_inner_size([420.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "myClipboardPlayList",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, instance)))),
    )
}
