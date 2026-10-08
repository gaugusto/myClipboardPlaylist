//! Garante uma única instância do app. A primeira instância escuta num socket;
//! as seguintes apenas pedem a ela que traga a janela para frente e encerram.

use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context as _, Result};

use crate::AppMsg;

const SOCKET_NAME: &str = "myclipboardplaylist.sock";

pub enum Instance {
    /// Esta é a primeira instância; o guard remove o socket ao encerrar.
    Primary(Guard),
    /// Já existe outra instância aberta (que foi avisada para aparecer).
    Secondary,
}

pub struct Guard {
    path: PathBuf,
    listener: Option<UnixListener>,
}

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join(SOCKET_NAME)
}

pub fn acquire() -> Result<Instance> {
    acquire_at(socket_path())
}

fn acquire_at(path: PathBuf) -> Result<Instance> {
    if let Ok(mut stream) = UnixStream::connect(&path) {
        let _ = stream.write_all(b"activate\n");
        return Ok(Instance::Secondary);
    }
    // Ninguém respondeu: socket órfão de uma execução encerrada à força (ou inexistente).
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("não foi possível criar {}", path.display()))?;
    Ok(Instance::Primary(Guard {
        path,
        listener: Some(listener),
    }))
}

impl Guard {
    /// Escuta pedidos de outras instâncias e os repassa à interface.
    pub fn listen(&mut self, tx: Sender<AppMsg>, ctx: eframe::egui::Context) {
        let Some(listener) = self.listener.take() else {
            return;
        };
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_err() {
                    continue;
                }
                if tx.send(AppMsg::Activate).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    fn temp_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mcpl-test-{}-{name}.sock", std::process::id()))
    }

    #[test]
    fn second_instance_activates_first() {
        let path = temp_socket("activate");
        let Instance::Primary(mut guard) = acquire_at(path.clone()).unwrap() else {
            panic!("deveria ser a primeira")
        };
        let (tx, rx) = channel();
        guard.listen(tx, eframe::egui::Context::default());

        assert!(matches!(
            acquire_at(path.clone()).unwrap(),
            Instance::Secondary
        ));
        let msg = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a primeira instância não foi avisada");
        assert!(matches!(msg, AppMsg::Activate));

        drop(guard);
        assert!(!path.exists(), "o socket deveria ser removido ao encerrar");
    }

    #[test]
    fn stale_socket_is_reused() {
        let path = temp_socket("stale");
        // Simula um app encerrado à força: o arquivo do socket fica, mas ninguém escuta.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        assert!(matches!(
            acquire_at(path.clone()).unwrap(),
            Instance::Primary(_)
        ));
    }
}
