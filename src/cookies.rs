use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

/// Navegador de onde o yt-dlp lê os cookies. Sem eles, o YouTube recusa alguns vídeos
/// ("Sign in to confirm you're not a bot"), tanto na consulta de metadados quanto no mpv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Brave,
    BraveOrigin,
    Chromium,
    Firefox,
}

/// Cookies que só existem com uma conta Google logada no YouTube.
const LOGIN_COOKIES: [&str; 3] = ["SID", "__Secure-3PSID", "LOGIN_INFO"];

/// Segundos entre 1601-01-01 (época dos cookies do Chromium) e 1970-01-01.
const CHROMIUM_EPOCH_OFFSET: u64 = 11_644_473_600;

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
    ///
    /// Sem um perfil, o yt-dlp lê o banco de cookies gravado por último, que pode ser o
    /// de um perfil sem login no YouTube. Por isso o perfil é escolhido aqui: o mais
    /// recente entre os que têm login. Se nenhum tiver, fica o comportamento do yt-dlp.
    pub fn ytdlp_arg(self) -> String {
        let profile = logged_in_profile(self.is_firefox(), &self.profile_dirs());
        let profile = match (profile, self) {
            (Some(dir), _) => Some(dir),
            // O yt-dlp não conhece o Brave Origin: é um Brave com outro diretório de perfis.
            (None, Browser::BraveOrigin) => self.profile_dirs().into_iter().next(),
            (None, _) => None,
        };
        let name = match self {
            Browser::Brave | Browser::BraveOrigin => "brave",
            Browser::Chromium => "chromium",
            Browser::Firefox => "firefox",
        };
        let keyring = if self.is_firefox() {
            ""
        } else {
            chromium_keyring()
        };
        match profile {
            Some(dir) => format!("{name}{keyring}:{}", dir.display()),
            None => format!("{name}{keyring}"),
        }
    }

    fn is_firefox(self) -> bool {
        self == Browser::Firefox
    }

    /// Diretórios que contêm os perfis do navegador.
    fn profile_dirs(self) -> Vec<PathBuf> {
        let config = config_dir();
        match self {
            Browser::Brave => vec![config.join("BraveSoftware/Brave-Browser")],
            Browser::BraveOrigin => vec![config.join("BraveSoftware/Brave-Origin")],
            Browser::Chromium => vec![config.join("chromium")],
            Browser::Firefox => {
                let home = std::env::var_os("HOME").map(PathBuf::from);
                home.map(|h| h.join(".mozilla/firefox"))
                    .into_iter()
                    .chain([config.join("mozilla/firefox")])
                    .collect()
            }
        }
    }
}

/// Perfil (diretório) com login no YouTube cujo banco de cookies mudou por último.
fn logged_in_profile(firefox: bool, roots: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for profile in roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
    {
        let Some(db) = cookie_db(firefox, &profile) else {
            continue;
        };
        let modified = std::fs::metadata(&db)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if best.as_ref().is_some_and(|(t, _)| *t >= modified) {
            continue;
        }
        match has_youtube_login(firefox, &db) {
            Ok(true) => best = Some((modified, profile)),
            Ok(false) => {}
            Err(e) => eprintln!("aviso: cookies em {}: {e}", db.display()),
        }
    }
    best.map(|(_, dir)| dir)
}

/// Banco de cookies do perfil, se houver.
fn cookie_db(firefox: bool, profile: &Path) -> Option<PathBuf> {
    let names: &[&str] = if firefox {
        &["cookies.sqlite"]
    } else {
        &["Cookies", "Network/Cookies"]
    };
    names.iter().map(|n| profile.join(n)).find(|p| p.is_file())
}

/// O banco tem cookies de login do YouTube ainda válidos.
fn has_youtube_login(firefox: bool, db: &Path) -> rusqlite::Result<bool> {
    // `immutable`: lê sem travar o banco, que o navegador mantém aberto.
    let uri = format!("file:{}?immutable=1", uri_path(db));
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (sql, now) = if firefox {
        // `expiry` está em segundos (ou em milissegundos nas versões novas, que também
        // passam na comparação).
        (
            "SELECT 1 FROM moz_cookies WHERE (host = 'youtube.com' OR host LIKE '%.youtube.com')
             AND name IN (?1, ?2, ?3) AND expiry > ?4 LIMIT 1",
            now as i64,
        )
    } else {
        // `expires_utc` está em microssegundos desde 1601; 0 é cookie de sessão.
        (
            "SELECT 1 FROM cookies WHERE (host_key = 'youtube.com' OR host_key LIKE '%.youtube.com')
             AND name IN (?1, ?2, ?3) AND (expires_utc = 0 OR expires_utc > ?4) LIMIT 1",
            ((now + CHROMIUM_EPOCH_OFFSET) * 1_000_000) as i64,
        )
    };
    let [a, b, c] = LOGIN_COOKIES;
    let mut stmt = conn.prepare(sql)?;
    stmt.exists(rusqlite::params![a, b, c, now])
}

/// Caminho escapado para uma URI `file:` do SQLite.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        match c {
            '?' | '#' | '%' => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
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

    /// Cria um banco de cookies do Chromium com os cookies `(host, nome, expira)`.
    fn chromium_db(path: &Path, cookies: &[(&str, &str, i64)]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE cookies (host_key TEXT, name TEXT, expires_utc INTEGER)",
            [],
        )
        .unwrap();
        for (host, name, expires) in cookies {
            conn.execute(
                "INSERT INTO cookies VALUES (?1, ?2, ?3)",
                rusqlite::params![host, name, expires],
            )
            .unwrap();
        }
    }

    #[test]
    fn picks_newest_profile_with_youtube_login() {
        let root = std::env::temp_dir().join(format!("mcpl-cookies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let far = i64::MAX;
        chromium_db(
            &root.join("Default/Cookies"),
            &[(".youtube.com", "SID", far)],
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        chromium_db(
            &root.join("Profile 2/Network/Cookies"),
            &[(".youtube.com", "LOGIN_INFO", 0)],
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        // O mais recente, mas sem login: cookie expirado e cookie de outro site.
        chromium_db(
            &root.join("Profile 1/Cookies"),
            &[(".youtube.com", "SID", 1), (".google.com", "SID", far)],
        );

        let found = logged_in_profile(false, std::slice::from_ref(&root));
        assert_eq!(found, Some(root.join("Profile 2")));

        std::fs::remove_dir_all(root.join("Default")).unwrap();
        std::fs::remove_dir_all(root.join("Profile 2")).unwrap();
        assert_eq!(logged_in_profile(false, std::slice::from_ref(&root)), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn falls_back_without_logged_in_profile() {
        assert!(Browser::Firefox.ytdlp_arg().starts_with("firefox"));
        let arg = Browser::BraveOrigin.ytdlp_arg();
        assert!(arg.starts_with("brave"));
        assert!(arg.contains("BraveSoftware/Brave-Origin"));
    }
}
