use std::path::PathBuf;

/// Navegador de onde o yt-dlp lê os cookies. Sem eles, o YouTube recusa alguns vídeos
/// ("Sign in to confirm you're not a bot"), tanto na consulta de metadados quanto no mpv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Brave,
    BraveOrigin,
    Chromium,
    Firefox,
}

impl Browser {
    /// Ordem do seletor.
    pub const ALL: [Browser; 4] = [
        Browser::Brave,
        Browser::BraveOrigin,
        Browser::Chromium,
        Browser::Firefox,
    ];

    /// Nome exibido e salvo nas configurações.
    pub fn name(self) -> &'static str {
        match self {
            Browser::Brave => "brave",
            Browser::BraveOrigin => "brave-origin",
            Browser::Chromium => "chromium",
            Browser::Firefox => "firefox",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    /// Valor de `--cookies-from-browser` do yt-dlp (`NAVEGADOR[+CHAVEIRO][:PERFIL]`).
    pub fn ytdlp_arg(self) -> String {
        match self {
            Browser::Brave => format!("brave{}", chromium_keyring()),
            // O yt-dlp não conhece o Brave Origin: é um Brave com outro diretório de perfis.
            Browser::BraveOrigin => format!(
                "brave{}:{}",
                chromium_keyring(),
                config_dir().join("BraveSoftware/Brave-Origin").display()
            ),
            Browser::Chromium => format!("chromium{}", chromium_keyring()),
            Browser::Firefox => "firefox".to_owned(),
        }
    }
}

/// Chaveiro que guarda a chave dos cookies dos navegadores baseados no Chromium. O yt-dlp
/// o deduz do ambiente gráfico, mas fora do GNOME e do KDE (niri, Hyprland, …) cai no
/// `BASICTEXT` e não consegue decifrar os cookies. Nesses ambientes, o navegador usa o
/// gnome-keyring (Secret Service).
fn chromium_keyring() -> &'static str {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if desktop.contains("KDE") {
        ""
    } else {
        "+gnomekeyring"
    }
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for browser in Browser::ALL {
            assert_eq!(Browser::from_name(browser.name()), Some(browser));
        }
        assert_eq!(Browser::from_name("opera"), None);
    }

    #[test]
    fn brave_origin_points_to_its_profile_dir() {
        let arg = Browser::BraveOrigin.ytdlp_arg();
        assert!(arg.starts_with("brave"));
        assert!(arg.ends_with("BraveSoftware/Brave-Origin"));
        assert_eq!(Browser::Firefox.ytdlp_arg(), "firefox");
    }
}
