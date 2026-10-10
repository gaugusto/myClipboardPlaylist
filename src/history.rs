use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use serde::Deserialize;

use crate::url::parse_link;

/// Um item de texto do histórico.
pub struct Clip {
    /// Identificador do item na fonte (usado para excluí-lo).
    pub id: u64,
    pub text: String,
}

/// Uma fonte de histórico da área de transferência (dms, cliphist, …).
pub trait HistorySource: Send + Sync {
    fn name(&self) -> &str;
    /// Itens de texto do histórico, do mais antigo para o mais recente.
    fn clips(&self) -> Result<Vec<Clip>>;
    /// Exclui um item do histórico.
    fn delete(&self, id: u64) -> Result<()>;
    /// Apaga todo o histórico.
    fn clear(&self) -> Result<()>;
    /// Processo que escreve uma linha na saída a cada mudança no clipboard.
    fn watch(&self) -> Result<Child>;
}

/// Lê o histórico da fonte e devolve os links válidos, sem repetições,
/// na ordem em que foram copiados.
pub fn fetch_links(source: &dyn HistorySource) -> Result<Vec<String>> {
    let mut links: Vec<String> = Vec::new();
    for link in source.clips()?.iter().filter_map(|c| parse_link(&c.text)) {
        if !links.contains(&link) {
            links.push(link);
        }
    }
    Ok(links)
}

/// Exclui do histórico todos os itens que são o link `link` (pode ter sido copiado
/// mais de uma vez). Devolve quantos foram excluídos.
pub fn delete_link(source: &dyn HistorySource, link: &str) -> Result<usize> {
    let ids: Vec<u64> = source
        .clips()?
        .into_iter()
        .filter(|c| parse_link(&c.text).as_deref() == Some(link))
        .map(|c| c.id)
        .collect();
    for &id in &ids {
        source.delete(id)?;
    }
    Ok(ids.len())
}

/// Espera antes de reabrir o processo de `HistorySource::watch` que terminou.
const WATCH_RETRY: Duration = Duration::from_secs(5);
/// Espera, depois de uma mudança no clipboard, para a fonte gravá-la no histórico.
const WATCH_SETTLE: Duration = Duration::from_millis(300);

/// Acompanha as mudanças no clipboard em segundo plano e chama `on_change` a cada uma.
/// O processo da fonte é encerrado quando o `Watcher` é descartado.
pub struct Watcher {
    state: Arc<Mutex<WatchState>>,
}

#[derive(Default)]
struct WatchState {
    child: Option<Child>,
    stopped: bool,
}

fn lock(state: &Mutex<WatchState>) -> MutexGuard<'_, WatchState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl Watcher {
    pub fn start(source: Arc<dyn HistorySource>, on_change: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(WatchState::default()));
        let shared = state.clone();
        thread::spawn(move || {
            loop {
                let stdout = {
                    let mut state = lock(&shared);
                    if state.stopped {
                        return;
                    }
                    // Sem como acompanhar (fonte não instalada): o histórico só é lido
                    // ao abrir o app.
                    let Ok(mut child) = source.watch() else {
                        return;
                    };
                    let stdout = child.stdout.take();
                    state.child = Some(child);
                    stdout
                };
                for line in stdout
                    .into_iter()
                    .flat_map(|out| BufReader::new(out).lines())
                {
                    if line.is_err() {
                        break;
                    }
                    thread::sleep(WATCH_SETTLE);
                    on_change();
                }
                if let Some(mut child) = lock(&shared).child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                thread::sleep(WATCH_RETRY);
            }
        });
        Self { state }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        state.stopped = true;
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Histórico do DankMaterialShell (`dms clipboard history --json`).
pub struct Dms;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DmsEntry {
    id: u64,
    #[serde(default)]
    is_image: bool,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    size: usize,
}

#[derive(Deserialize)]
struct DmsFullEntry {
    data: String,
}

fn run_dms(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("dms")
        .args(args)
        .output()
        .context("não foi possível executar `dms` (está instalado?)")?;
    if !output.status.success() {
        bail!(
            "`dms {}` falhou: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

impl Dms {
    /// O `preview` pode vir truncado; nesse caso busca o conteúdo completo.
    fn full_text(entry: &DmsEntry) -> Result<String> {
        let out = run_dms(&["clipboard", "get", &entry.id.to_string(), "--json"])?;
        let full: DmsFullEntry = serde_json::from_slice(&out)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(full.data.trim())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl HistorySource for Dms {
    fn name(&self) -> &str {
        "dms"
    }

    fn clips(&self) -> Result<Vec<Clip>> {
        let out = run_dms(&["clipboard", "history", "--json"])?;
        let entries: Vec<DmsEntry> = serde_json::from_slice(&out)
            .context("saída inesperada de `dms clipboard history --json`")?;
        let mut clips = Vec::with_capacity(entries.len());
        // O dms lista do mais recente para o mais antigo.
        for entry in entries.iter().rev().filter(|e| !e.is_image) {
            let truncated = entry.preview.len() < entry.size;
            let text = if truncated && entry.preview.trim_start().starts_with("http") {
                Self::full_text(entry)?
            } else {
                entry.preview.clone()
            };
            clips.push(Clip { id: entry.id, text });
        }
        Ok(clips)
    }

    fn delete(&self, id: u64) -> Result<()> {
        run_dms(&["clipboard", "delete", &id.to_string()]).map(drop)
    }

    fn clear(&self) -> Result<()> {
        run_dms(&["clipboard", "clear"]).map(drop)
    }

    fn watch(&self) -> Result<Child> {
        Command::new("dms")
            .args(["clipboard", "watch", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("não foi possível executar `dms clipboard watch`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// Histórico em memória; o id de cada item é a sua posição inicial.
    struct Fake(Mutex<Vec<(u64, &'static str)>>);

    impl Fake {
        fn new(texts: Vec<&'static str>) -> Self {
            Self(Mutex::new((0..).zip(texts).collect()))
        }
    }

    impl HistorySource for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn clips(&self) -> Result<Vec<Clip>> {
            let items = self.0.lock().unwrap();
            Ok(items
                .iter()
                .map(|&(id, text)| Clip {
                    id,
                    text: text.to_owned(),
                })
                .collect())
        }
        fn delete(&self, id: u64) -> Result<()> {
            self.0.lock().unwrap().retain(|&(i, _)| i != id);
            Ok(())
        }
        fn clear(&self) -> Result<()> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
        fn watch(&self) -> Result<Child> {
            bail!("sem suporte")
        }
    }

    fn sample() -> Fake {
        Fake::new(vec![
            "https://youtu.be/a",
            "texto qualquer",
            "https://youtu.be/b",
            "https://youtu.be/a",
        ])
    }

    #[test]
    fn filters_and_dedups_in_order() {
        assert_eq!(
            fetch_links(&sample()).unwrap(),
            vec!["https://youtu.be/a", "https://youtu.be/b"]
        );
    }

    #[test]
    fn deletes_every_copy_of_a_link() {
        let source = sample();
        assert_eq!(delete_link(&source, "https://youtu.be/a").unwrap(), 2);
        assert_eq!(fetch_links(&source).unwrap(), vec!["https://youtu.be/b"]);
        assert_eq!(source.clips().unwrap().len(), 2);
    }
}
