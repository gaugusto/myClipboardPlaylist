use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AppMsg, Notifier};

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    /// Identificador único do item na playlist do mpv (a URL pode se repetir).
    pub id: u64,
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub current: bool,
}

/// Por que um vídeo não pôde ser reproduzido.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// Explicação para o usuário.
    pub reason: String,
    /// Mensagem original (do yt-dlp ou do mpv).
    pub detail: String,
}

#[derive(Debug)]
pub enum MpvEvent {
    Playlist(Vec<Entry>),
    Title(Option<String>),
    /// `true` quando o mpv está ocioso (nada tocando).
    Idle(bool),
    /// `true` quando a reprodução está pausada.
    Pause(bool),
    /// Um item não pôde ser reproduzido (`id`: o item da playlist). O mpv segue para o próximo.
    FileError(Option<u64>, Failure),
    /// A reprodução começou (ou recomeçou, depois de um salto).
    PlaybackStarted,
    Exited,
}

/// Nome fixo do socket: permite reconectar ao mpv que continuou tocando
/// depois que o app foi fechado.
const SOCKET_NAME: &str = "myclipboardplaylist-mpv.sock";

/// Mantém um único processo mpv e o controla via JSON IPC.
/// O mpv sobrevive ao app: ao reabrir, o app se reconecta a ele.
pub struct Mpv {
    socket: PathBuf,
    /// Presente só quando este app iniciou o mpv (para recolher o processo).
    child: Option<Child>,
    writer: Option<UnixStream>,
    /// Fica `false` quando a conexão atual com o mpv é encerrada.
    alive: Arc<AtomicBool>,
    /// Valor de `--cookies-from-browser` repassado ao yt-dlp pelo mpv.
    cookies: Option<String>,
    notifier: Notifier,
}

impl Mpv {
    pub fn new(notifier: Notifier) -> Self {
        let dir =
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        Self {
            socket: dir.join(SOCKET_NAME),
            child: None,
            writer: None,
            alive: Arc::new(AtomicBool::new(false)),
            cookies: None,
            notifier,
        }
    }

    fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child
            && !matches!(child.try_wait(), Ok(None))
        {
            self.child = None;
        }
        self.writer.is_some() && self.alive.load(Ordering::Relaxed)
    }

    /// Conecta a um mpv deixado aberto por uma execução anterior do app
    /// e devolve a playlist dele. `None` se não houver nenhum.
    pub fn attach(&mut self) -> Option<Vec<Entry>> {
        let stream = UnixStream::connect(&self.socket).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        (&stream)
            .write_all(b"{\"command\": [\"get_property\", \"playlist\"], \"request_id\": 1}\n")
            .ok()?;
        let playlist = loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg.get("request_id").and_then(Value::as_u64) == Some(1) {
                break serde_json::from_value(msg.get("data")?.clone()).ok()?;
            }
        };
        stream.set_read_timeout(None).ok()?;
        self.start(stream, reader).ok()?;
        // Volta a ficar aberto entre um vídeo e outro enquanto o app estiver aberto.
        self.command(json!(["set_property", "idle", "yes"])).ok()?;
        Some(playlist)
    }

    /// Liga a thread leitora de eventos e observa as propriedades usadas pela interface.
    fn start(&mut self, stream: UnixStream, reader: BufReader<UnixStream>) -> Result<()> {
        let alive = Arc::new(AtomicBool::new(true));
        self.alive = alive.clone();
        let notifier = self.notifier.clone();
        thread::spawn(move || read_events(reader, &alive, &notifier));
        self.writer = Some(stream);
        // A causa de uma falha só aparece no log: o `end-file` traz um texto genérico.
        self.command(json!(["request_log_messages", "warn"]))?;
        for (id, prop) in ["playlist", "media-title", "idle-active", "pause"]
            .iter()
            .enumerate()
        {
            self.command(json!(["observe_property", id + 1, prop]))?;
        }
        self.apply_cookies()
    }

    /// Define de qual navegador o yt-dlp do mpv lê os cookies (`None`: de nenhum).
    pub fn set_cookies(&mut self, cookies: Option<String>) -> Result<()> {
        self.cookies = cookies;
        if self.is_running() {
            self.apply_cookies()
        } else {
            Ok(())
        }
    }

    /// Mexe só na chave `cookies-from-browser` de `ytdl-raw-options`, preservando o que
    /// vier do `mpv.conf`. O ytdl_hook lê a opção a cada arquivo aberto.
    fn apply_cookies(&mut self) -> Result<()> {
        let cmd = match &self.cookies {
            Some(arg) => json!([
                "change-list",
                "ytdl-raw-options",
                "append",
                format!("cookies-from-browser={arg}")
            ]),
            None => json!([
                "change-list",
                "ytdl-raw-options",
                "remove",
                "cookies-from-browser"
            ]),
        };
        self.command(cmd)
    }

    fn ensure_running(&mut self) -> Result<()> {
        if self.is_running() || self.attach().is_some() {
            return Ok(());
        }
        // Socket órfão de um mpv que já não existe.
        let _ = std::fs::remove_file(&self.socket);

        let child = Command::new("mpv")
            .arg("--idle=yes")
            .arg("--title=Clipboard Playlist - ${media-title}")
            .arg(format!("--input-ipc-server={}", self.socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Grupo de processos próprio: um Ctrl+C no terminal do app não derruba o mpv.
            .process_group(0)
            .spawn()
            .context("não foi possível iniciar o mpv (está instalado?)")?;
        self.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(&self.socket) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    if let Some(mut child) = self.child.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    bail!("mpv não abriu o socket IPC: {e}");
                }
            }
        };
        let reader = BufReader::new(stream.try_clone()?);
        self.start(stream, reader)
    }

    /// Chamado ao fechar o app. Se algo estiver tocando, o mpv continua e fecha sozinho
    /// quando a fila terminar; se estiver ocioso, é encerrado (não teria janela visível).
    pub fn detach(&mut self, idle: bool) {
        if !self.is_running() {
            return;
        }
        let cmd = if idle {
            json!(["quit"])
        } else {
            json!(["set_property", "idle", "no"])
        };
        let _ = self.command(cmd);
        self.writer = None;
    }

    fn command(&mut self, cmd: Value) -> Result<()> {
        let writer = self.writer.as_mut().context("mpv não está em execução")?;
        let mut line = serde_json::to_vec(&json!({ "command": cmd }))?;
        line.push(b'\n');
        if let Err(e) = writer.write_all(&line) {
            self.writer = None;
            return Err(e).context("falha ao enviar comando ao mpv");
        }
        Ok(())
    }

    /// Envia um comando apenas se o mpv já estiver aberto.
    fn command_if_running(&mut self, cmd: Value) -> Result<()> {
        if self.is_running() {
            self.command(cmd)
        } else {
            Ok(())
        }
    }

    /// Acrescenta `links` ao fim da fila, sem iniciar a reprodução.
    pub fn append(&mut self, links: &[String]) -> Result<()> {
        if links.is_empty() {
            return Ok(());
        }
        self.ensure_running()?;
        for url in links {
            self.command(json!(["loadfile", url, "append"]))?;
        }
        Ok(())
    }

    /// Abre a fila `links` de uma vez (um só evento de playlist), sem iniciar a reprodução.
    /// Usado para refazer a fila num mpv novo, depois que o anterior foi fechado.
    pub fn restore(&mut self, links: &[String]) -> Result<()> {
        self.ensure_running()?;
        let list = format!("memory://#EXTM3U\n{}\n", links.join("\n"));
        self.command(json!(["loadlist", list, "append"]))
    }

    /// Move o item `from` para antes do item `to` (`to` igual ao tamanho da fila: para o
    /// fim). O item que está tocando continua tocando.
    pub fn move_entry(&mut self, from: usize, to: usize) -> Result<()> {
        self.command_if_running(json!(["playlist-move", from, to]))
    }

    pub fn play_index(&mut self, index: usize) -> Result<()> {
        self.command_if_running(json!(["playlist-play-index", index]))?;
        self.command_if_running(json!(["set_property", "pause", false]))
    }

    pub fn set_pause(&mut self, pause: bool) -> Result<()> {
        self.command_if_running(json!(["set_property", "pause", pause]))
    }

    pub fn remove(&mut self, index: usize) -> Result<()> {
        self.command_if_running(json!(["playlist-remove", index]))
    }

    /// Para a reprodução e esvazia a fila.
    pub fn clear(&mut self) -> Result<()> {
        // Sem `keep-playlist`, o `stop` também esvazia a playlist, mas, com o mpv ocioso,
        // não avisa quem observa a propriedade `playlist`. O `playlist-clear` sempre avisa.
        self.command_if_running(json!(["stop"]))?;
        self.command_if_running(json!(["playlist-clear"]))
    }
}

fn read_events(reader: BufReader<UnixStream>, alive: &AtomicBool, notifier: &Notifier) {
    // Causa da falha do arquivo atual, vinda do log do mpv.
    let mut cause = None;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(event) = parse_event(&msg, &mut cause)
            && !notifier.send(AppMsg::Mpv(event))
        {
            return;
        }
    }
    alive.store(false, Ordering::Relaxed);
    notifier.send(AppMsg::Mpv(MpvEvent::Exited));
}

fn parse_event(msg: &Value, cause: &mut Option<String>) -> Option<MpvEvent> {
    match msg.get("event")?.as_str()? {
        "property-change" => {
            let data = msg.get("data").cloned().unwrap_or(Value::Null);
            match msg.get("name")?.as_str()? {
                "playlist" => serde_json::from_value(data).ok().map(MpvEvent::Playlist),
                "media-title" => Some(MpvEvent::Title(data.as_str().map(String::from))),
                "idle-active" => Some(MpvEvent::Idle(data.as_bool().unwrap_or(true))),
                "pause" => Some(MpvEvent::Pause(data.as_bool().unwrap_or(false))),
                _ => None,
            }
        }
        "playback-restart" => Some(MpvEvent::PlaybackStarted),
        "start-file" => {
            *cause = None;
            None
        }
        "log-message" => {
            note_log(msg, cause);
            None
        }
        "end-file" if msg.get("reason").and_then(Value::as_str) == Some("error") => {
            let id = msg.get("playlist_entry_id").and_then(Value::as_u64);
            let file_error = msg.get("file_error").and_then(Value::as_str);
            Some(MpvEvent::FileError(id, failure(cause.take(), file_error)))
        }
        _ => None,
    }
}

/// Linhas de erro do log que só repetem que algo falhou, sem dizer o quê.
const GENERIC_LOGS: &[&str] = &["youtube-dl failed", "Failed to open", "Failed to recognize"];

/// Guarda em `cause` a linha do log que melhor explica a falha do arquivo atual: um `ERROR:`
/// do yt-dlp ou, se não houver, a primeira linha de erro. Avisos são ignorados, exceto os
/// erros HTTP do ffmpeg, que o mpv registra só como aviso.
fn note_log(msg: &Value, cause: &mut Option<String>) {
    let field = |name| msg.get(name).and_then(Value::as_str).unwrap_or_default();
    let text = field("text").trim();
    let is_error = matches!(field("level"), "error" | "fatal");
    let is_http = field("prefix") == "ffmpeg" && text.contains("HTTP error");
    if !(is_error || is_http) || GENERIC_LOGS.iter().any(|g| text.starts_with(g)) {
        return;
    }
    match text.strip_prefix("ERROR: ") {
        Some(rest) => *cause = Some(strip_extractor(rest).to_owned()),
        None => {
            cause.get_or_insert_with(|| text.to_owned());
        }
    }
}

/// Tira o começo `[youtube] abc123: ` das mensagens do yt-dlp.
fn strip_extractor(text: &str) -> &str {
    text.strip_prefix('[')
        .and_then(|rest| rest.split_once(": "))
        .map_or(text, |(_, message)| message)
}

/// Monta a explicação de uma falha a partir do log (`cause`) e do `file_error` do mpv.
fn failure(cause: Option<String>, file_error: Option<&str>) -> Failure {
    let detail = cause
        .or_else(|| file_error.map(String::from))
        .unwrap_or_else(|| "erro desconhecido".to_owned());
    let text = detail.to_lowercase();
    let has = |terms: &[&str]| terms.iter().any(|t| text.contains(t));
    let reason = if has(&["sign in", "log in", "login", "cookies"]) {
        "o site pediu login; escolha o navegador em Cookies"
    } else if has(&[
        "private video",
        "unavailable",
        "removed",
        "not available in your",
    ]) {
        "o vídeo não está disponível"
    } else if has(&["requested format"]) {
        "nenhum formato disponível; atualize o yt-dlp"
    } else if has(&["403"]) {
        "o site recusou o acesso (HTTP 403); atualize o yt-dlp"
    } else if has(&["429"]) {
        "o site limitou os acessos (HTTP 429); tente mais tarde"
    } else if has(&["no audio or video data played", "ends prematurely"]) {
        "o vídeo abriu, mas não chegou áudio nem imagem; tente de novo ou atualize o yt-dlp"
    } else if has(&["timed out", "connection", "resolve", "network"]) {
        "falha de conexão; verifique a internet"
    } else if has(&["unrecognized file format", "unsupported url"]) {
        "o link não tem um vídeo reconhecido"
    } else {
        return Failure {
            reason: detail.clone(),
            detail,
        };
    };
    Failure {
        reason: reason.to_owned(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(events: &[Value]) -> Option<MpvEvent> {
        let mut cause = None;
        events.iter().fold(None, |_, e| parse_event(e, &mut cause))
    }

    fn log(prefix: &str, level: &str, text: &str) -> Value {
        json!({"event": "log-message", "prefix": prefix, "level": level, "text": text})
    }

    fn end_file(error: &str) -> Value {
        json!({"event": "end-file", "reason": "error", "playlist_entry_id": 7, "file_error": error})
    }

    #[test]
    fn failure_uses_ytdlp_error_from_log() {
        let event = feed(&[
            log("ffmpeg", "warn", "http: HTTP error 403 Forbidden\n"),
            log(
                "ytdl_hook",
                "error",
                "ERROR: [youtube] abc: Sign in to confirm you're not a bot\n",
            ),
            log(
                "ytdl_hook",
                "error",
                "youtube-dl failed: unexpected error\n",
            ),
            end_file("unrecognized file format"),
        ]);
        let Some(MpvEvent::FileError(id, failure)) = event else {
            panic!("esperava FileError");
        };
        assert_eq!(id, Some(7));
        assert_eq!(failure.detail, "Sign in to confirm you're not a bot");
        assert!(failure.reason.contains("Cookies"));
    }

    #[test]
    fn failure_uses_http_warning() {
        let event = feed(&[
            log("mkv", "warn", "Unexpected end of file\n"),
            log("ffmpeg", "warn", "http: HTTP error 403 Forbidden\n"),
            end_file("no audio or video data played"),
        ]);
        let Some(MpvEvent::FileError(_, failure)) = event else {
            panic!("esperava FileError");
        };
        assert!(failure.reason.contains("403"));
    }

    #[test]
    fn failure_falls_back_to_file_error() {
        let event = feed(&[end_file("no audio or video data played")]);
        let Some(MpvEvent::FileError(_, failure)) = event else {
            panic!("esperava FileError");
        };
        assert_eq!(failure.detail, "no audio or video data played");
        assert!(failure.reason.contains("nem imagem"));
    }

    #[test]
    fn log_of_previous_file_is_forgotten() {
        let event = feed(&[
            log("ytdl_hook", "error", "ERROR: [youtube] a: Private video\n"),
            json!({"event": "start-file", "playlist_entry_id": 8}),
            end_file("loading failed"),
        ]);
        let Some(MpvEvent::FileError(_, failure)) = event else {
            panic!("esperava FileError");
        };
        assert_eq!(failure.detail, "loading failed");
        assert_eq!(failure.reason, "loading failed");
    }
}
