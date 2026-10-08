use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppMsg;

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug)]
pub enum MpvEvent {
    Playlist(Vec<Entry>),
    Title(Option<String>),
    /// `true` quando o mpv está ocioso (nada tocando).
    Idle(bool),
    FileError(String),
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
    tx: Sender<AppMsg>,
    ctx: eframe::egui::Context,
}

impl Mpv {
    pub fn new(tx: Sender<AppMsg>, ctx: eframe::egui::Context) -> Self {
        let dir =
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        Self {
            socket: dir.join(SOCKET_NAME),
            child: None,
            writer: None,
            alive: Arc::new(AtomicBool::new(false)),
            tx,
            ctx,
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
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || read_events(reader, &alive, &tx, &ctx));
        self.writer = Some(stream);
        for (id, prop) in ["playlist", "media-title", "idle-active"]
            .iter()
            .enumerate()
        {
            self.command(json!(["observe_property", id + 1, prop]))?;
        }
        Ok(())
    }

    fn ensure_running(&mut self) -> Result<()> {
        if self.is_running() || self.attach().is_some() {
            return Ok(());
        }
        // Socket órfão de um mpv que já não existe.
        let _ = std::fs::remove_file(&self.socket);

        let child = Command::new("mpv")
            .arg("--idle=yes")
            .arg("--title=myClipboardPlayList - ${media-title}")
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

    /// Substitui a playlist inteira por `links`, sem iniciar a reprodução.
    /// Se o item atual (`current`) estiver em `links`, ele não é interrompido:
    /// os demais são reconstruídos ao redor dele.
    pub fn replace(&mut self, links: &[String], current: Option<&str>) -> Result<()> {
        self.ensure_running()?;
        let keep = current.and_then(|c| links.iter().position(|l| l == c));
        match keep {
            // playlist-clear remove tudo exceto o item atual, que fica no índice 0.
            Some(_) => self.command(json!(["playlist-clear"]))?,
            None => self.command(json!(["stop"]))?,
        }
        for (i, url) in links.iter().enumerate() {
            if Some(i) != keep {
                self.command(json!(["loadfile", url, "append"]))?;
            }
        }
        if let Some(k) = keep.filter(|&k| k > 0) {
            // Move o item atual (índice 0) para a posição dele na nova fila.
            self.command(json!(["playlist-move", 0, k + 1]))?;
        }
        Ok(())
    }

    pub fn play_index(&mut self, index: usize) -> Result<()> {
        self.command_if_running(json!(["playlist-play-index", index]))?;
        self.command_if_running(json!(["set_property", "pause", false]))
    }

    pub fn remove(&mut self, index: usize) -> Result<()> {
        self.command_if_running(json!(["playlist-remove", index]))
    }
}

fn read_events(
    reader: BufReader<UnixStream>,
    alive: &AtomicBool,
    tx: &Sender<AppMsg>,
    ctx: &eframe::egui::Context,
) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(event) = parse_event(&msg) {
            if tx.send(AppMsg::Mpv(event)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
    alive.store(false, Ordering::Relaxed);
    let _ = tx.send(AppMsg::Mpv(MpvEvent::Exited));
    ctx.request_repaint();
}

fn parse_event(msg: &Value) -> Option<MpvEvent> {
    match msg.get("event")?.as_str()? {
        "property-change" => {
            let data = msg.get("data").cloned().unwrap_or(Value::Null);
            match msg.get("name")?.as_str()? {
                "playlist" => serde_json::from_value(data).ok().map(MpvEvent::Playlist),
                "media-title" => Some(MpvEvent::Title(data.as_str().map(String::from))),
                "idle-active" => Some(MpvEvent::Idle(data.as_bool().unwrap_or(true))),
                _ => None,
            }
        }
        "end-file" if msg.get("reason").and_then(Value::as_str) == Some("error") => {
            let err = msg
                .get("file_error")
                .and_then(Value::as_str)
                .unwrap_or("erro desconhecido");
            Some(MpvEvent::FileError(format!("Falha ao reproduzir: {err}")))
        }
        _ => None,
    }
}
