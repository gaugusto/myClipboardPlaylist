use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::AppMsg;

/// Quantas consultas ao yt-dlp podem rodar ao mesmo tempo.
const WORKERS: usize = 4;
/// Largura mínima desejada para a thumbnail (a lista exibe ~128 px).
const THUMB_MIN_WIDTH: u32 = 240;

#[derive(Debug, Clone)]
pub struct Metadata {
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub is_live: bool,
    pub thumbnail: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    title: Option<String>,
    duration: Option<f64>,
    is_live: Option<bool>,
    thumbnail: Option<String>,
    #[serde(default)]
    thumbnails: Option<Vec<Thumb>>,
}

#[derive(Deserialize)]
struct Thumb {
    url: String,
    width: Option<u32>,
}

/// Prefere a menor thumbnail com largura conhecida ≥ `THUMB_MIN_WIDTH`;
/// senão usa a thumbnail principal.
fn pick_thumbnail(raw: &Raw) -> Option<String> {
    raw.thumbnails
        .iter()
        .flatten()
        .filter(|t| t.width.is_some_and(|w| w >= THUMB_MIN_WIDTH))
        .min_by_key(|t| t.width)
        .map(|t| t.url.clone())
        .or_else(|| raw.thumbnail.clone())
}

fn fetch(url: &str) -> Result<Metadata> {
    let output = Command::new("yt-dlp")
        .args(["--no-warnings", "--no-playlist", "--print"])
        .arg("%(.{title,duration,is_live,thumbnail,thumbnails})j")
        .arg("--")
        .arg(url)
        .output()
        .context("não foi possível executar o yt-dlp")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let raw: Raw = serde_json::from_slice(&output.stdout).context("saída inesperada do yt-dlp")?;
    Ok(Metadata {
        thumbnail: pick_thumbnail(&raw),
        title: raw.title,
        duration: raw.duration,
        is_live: raw.is_live.unwrap_or(false),
    })
}

/// Formata segundos como `m:ss` ou `h:mm:ss`.
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Pool de threads que consulta o yt-dlp e envia os resultados como `AppMsg::Metadata`.
pub struct Fetcher {
    queue: Sender<String>,
}

impl Fetcher {
    pub fn new(tx: Sender<AppMsg>, ctx: eframe::egui::Context) -> Self {
        let (queue, jobs) = channel::<String>();
        let jobs = Arc::new(Mutex::new(jobs));
        for _ in 0..WORKERS {
            let (jobs, tx, ctx) = (jobs.clone(), tx.clone(), ctx.clone());
            thread::spawn(move || worker(&jobs, &tx, &ctx));
        }
        Self { queue }
    }

    pub fn request(&self, url: String) {
        let _ = self.queue.send(url);
    }
}

fn worker(jobs: &Mutex<Receiver<String>>, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
    loop {
        let Ok(url) = jobs
            .lock()
            .map_err(drop)
            .and_then(|rx| rx.recv().map_err(drop))
        else {
            return;
        };
        let result = fetch(&url).map_err(|e| format!("{e:#}"));
        if tx.send(AppMsg::Metadata(url, result)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(5.0), "0:05");
        assert_eq!(format_duration(2191.0), "36:31");
        assert_eq!(format_duration(3725.4), "1:02:05");
    }

    /// Usa a rede e o yt-dlp instalado: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_youtube_metadata() {
        let meta = fetch("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        assert!(meta.title.is_some_and(|t| !t.is_empty()));
        assert!(meta.duration.is_some_and(|d| d > 0.0));
        assert!(meta.thumbnail.is_some_and(|t| t.starts_with("https://")));
    }

    #[test]
    fn picks_smallest_large_enough_thumbnail() {
        let raw: Raw = serde_json::from_str(
            r#"{"thumbnail": "big.webp", "thumbnails": [
                {"url": "a.jpg"}, {"url": "b.jpg", "width": 168},
                {"url": "c.jpg", "width": 1280}, {"url": "d.jpg", "width": 246}]}"#,
        )
        .unwrap();
        assert_eq!(pick_thumbnail(&raw).as_deref(), Some("d.jpg"));
        let raw: Raw = serde_json::from_str(r#"{"thumbnail": "big.webp"}"#).unwrap();
        assert_eq!(pick_thumbnail(&raw).as_deref(), Some("big.webp"));
    }
}
