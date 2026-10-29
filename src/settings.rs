//! Configurações salvas entre execuções, em `$XDG_CONFIG_HOME/myclipboardplaylist/settings.json`.
//! Na primeira execução, migra os valores que a versão em egui guardava no `app.ron` do eframe.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

const DIR_NAME: &str = "myclipboardplaylist";
const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Rodapé com a ajuda de atalhos visível.
    pub show_help: bool,
    /// Ordem da fila (URLs), para refazê-la na mesma ordem quando o mpv já fechou.
    pub queue_order: Vec<String>,
    /// Navegador dos cookies do yt-dlp (vazio: nenhum).
    pub cookies_browser: String,
    /// Tamanho da janela, em pixels lógicos.
    pub window_size: Option<(i32, i32)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_help: true,
            queue_order: Vec::new(),
            cookies_browser: String::new(),
            window_size: None,
        }
    }
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))
        .unwrap_or_else(std::env::temp_dir)
}

fn path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
        .join(DIR_NAME)
        .join(FILE_NAME)
}

/// Arquivo onde a versão em egui (eframe) guardava as configurações.
fn eframe_path() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
        .join(DIR_NAME)
        .join("app.ron")
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(&path(), &eframe_path())
    }

    fn load_from(path: &Path, eframe: &Path) -> Self {
        if let Ok(text) = std::fs::read_to_string(path) {
            return serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("aviso: configurações inválidas em {}: {e}", path.display());
                Self::default()
            });
        }
        std::fs::read_to_string(eframe)
            .ok()
            .and_then(|text| Self::from_eframe(&text))
            .unwrap_or_default()
    }

    /// Lê o `app.ron` do eframe: um mapa RON em que cada valor é um texto. Fora a vírgula
    /// depois do último item, que o RON aceita, é JSON.
    fn from_eframe(text: &str) -> Option<Self> {
        let body = text.trim_end().strip_suffix('}')?.trim_end();
        let json = format!("{}}}", body.strip_suffix(',').unwrap_or(body));
        let map: std::collections::HashMap<String, String> = serde_json::from_str(&json).ok()?;
        let mut settings = Self::default();
        if let Some(show) = map.get("show_help") {
            settings.show_help = show != "false";
        }
        if let Some(order) = map.get("queue_order") {
            settings.queue_order = serde_json::from_str(order).unwrap_or_default();
        }
        if let Some(browser) = map.get("cookies_browser") {
            settings.cookies_browser = browser.clone();
        }
        settings.window_size = map.get("window").and_then(|w| eframe_size(w));
        Some(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("não foi possível criar {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // Grava num arquivo temporário e renomeia, para não deixar o arquivo pela metade.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .with_context(|| format!("não foi possível salvar {}", path.display()))
    }
}

/// Tamanho da janela no formato RON do eframe: `inner_size_points:Some((x:683.0,y:1390.0))`.
fn eframe_size(window: &str) -> Option<(i32, i32)> {
    let rest = &window[window.find("inner_size_points:Some((")? + 24..];
    let rest = &rest[..rest.find("))")?];
    let mut x = None;
    let mut y = None;
    for part in rest.split(',') {
        let (key, value) = part.split_once(':')?;
        let value = value.trim().parse::<f32>().ok()?.round() as i32;
        match key.trim() {
            "x" => x = Some(value),
            "y" => y = Some(value),
            _ => {}
        }
    }
    Some((x?, y?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_eframe_storage() {
        let ron = r#"{
            "theme": "Meia-noite",
            "queue_order": "[\"https://a\",\"https://b\"]",
            "show_help": "false",
            "cookies_browser": "firefox",
            "window": "(inner_position_pixels:None,fullscreen:false,inner_size_points:Some((x:683.0,y:1390.0)))",
        }
"#;
        let settings = Settings::from_eframe(ron).unwrap();
        assert!(!settings.show_help);
        assert_eq!(settings.queue_order, ["https://a", "https://b"]);
        assert_eq!(settings.cookies_browser, "firefox");
        assert_eq!(settings.window_size, Some((683, 1390)));
    }

    #[test]
    fn prefers_own_file_and_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("mcpl-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (own, eframe) = (dir.join("settings.json"), dir.join("app.ron"));
        assert_eq!(Settings::load_from(&own, &eframe), Settings::default());
        std::fs::write(&eframe, r#"{"show_help": "false"}"#).unwrap();
        assert!(!Settings::load_from(&own, &eframe).show_help);
        std::fs::write(&own, r#"{"cookies_browser": "brave"}"#).unwrap();
        let settings = Settings::load_from(&own, &eframe);
        assert!(settings.show_help);
        assert_eq!(settings.cookies_browser, "brave");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
