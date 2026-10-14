//! One emulator run at a time: spawns qemu-system-*, listens for its QMP and
//! eink display connections, streams the console and the panel to the
//! WebView, and brackets SD card edits with stop / reset / cont.

use crate::display;
use crate::qmp::Qmp;
use crate::sd::SdError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

/// An error for the WebView: a code (fat.js codes for the SD card, EBUSY,
/// EQEMU, …) and a message for people.
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
    pub fn busy() -> Self {
        Self::new("EBUSY", "The emulator is busy; try again in a moment.")
    }
}

impl From<SdError> for AppError {
    fn from(error: SdError) -> Self {
        Self::new(error.code, error.message)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::new("EIO", error.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    /// "stopped", "failed" (could not start) or "crashed" (ended while running).
    pub kind: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// "idle", "starting", "running" or "stopping".
    pub state: &'static str,
    pub machine: Option<String>,
    pub turbo: bool,
    pub exit: Option<Exit>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    pub machine: String,
    pub panel: String,
    pub wifi: bool,
    pub turbo: bool,
    pub ghosting: bool,
}

/// Where the QEMU binaries are, and the -L data directory when packaged.
#[derive(Debug, Clone)]
pub struct QemuPaths {
    pub bin: PathBuf,
    pub share: Option<PathBuf>,
}

impl QemuPaths {
    pub fn binary(&self, machine: &str) -> PathBuf {
        let name = if machine == "x3" || machine == "x4" { "qemu-system-riscv32" } else { "qemu-system-xtensa" };
        self.bin.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() })
    }
}

const TURBO_ARGS: &str = include_str!("../resources/turbo-args.json");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_INTERVAL: Duration = Duration::from_millis(30);

struct Run {
    id: u64,
    machine: String,
    qmp: Qmp,
    input: mpsc::UnboundedSender<Input>,
    kill: StdMutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<Option<String>>,
    stopping: AtomicBool,
    _dir: tempfile::TempDir,
}

enum Input {
    Key(String, bool),
    Touch(f64, f64, bool),
}

struct Shared {
    info: SessionInfo,
    run: Option<Arc<Run>>,
    cancel: Option<watch::Sender<bool>>,
}

struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub struct Session {
    app: AppHandle,
    qemu: QemuPaths,
    /// Serialises start, stop, reset and SD card operations.
    ops: Mutex<()>,
    shared: StdMutex<Shared>,
    runs: AtomicU64,
    log_tx: mpsc::UnboundedSender<String>,
    logs: Arc<StdMutex<Option<Channel<Vec<String>>>>>,
    frames: StdMutex<Option<Channel<InvokeResponseBody>>>,
    screen: StdMutex<Option<Framebuffer>>,
    card: StdMutex<PathBuf>,
}

fn escape(value: &str) -> String {
    value.replace(',', ",,")
}

fn describe(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return format!("signal {signal}");
                }
            }
            match status.code() {
                Some(code) => format!("exit code {code}"),
                None => "unknown status".into(),
            }
        }
        Err(error) => error.to_string(),
    }
}

impl Session {
    pub fn new(app: AppHandle, qemu: QemuPaths, card: PathBuf) -> Arc<Session> {
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<String>();
        let logs: Arc<StdMutex<Option<Channel<Vec<String>>>>> = Arc::default();
        let sink = logs.clone();
        // Console lines are batched so a chatty firmware costs one IPC message per tick.
        // EINK_QEMU_LOG: also append console lines to a file (tests).
        let mut log_file = std::env::var_os("EINK_QEMU_LOG").and_then(|path| {
            std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
        });
        tauri::async_runtime::spawn(async move {
            let mut batch = Vec::new();
            while log_rx.recv_many(&mut batch, 4096).await > 0 {
                tokio::time::sleep(LOG_INTERVAL).await;
                while let Ok(line) = log_rx.try_recv() {
                    batch.push(line);
                }
                if let Some(file) = log_file.as_mut() {
                    use std::io::Write;
                    let _ = file.write_all((batch.join("\n") + "\n").as_bytes());
                }
                if let Some(channel) = sink.lock().unwrap().as_ref() {
                    let _ = channel.send(std::mem::take(&mut batch));
                }
                batch.clear();
            }
        });
        Arc::new(Session {
            app,
            qemu,
            ops: Mutex::new(()),
            shared: StdMutex::new(Shared {
                info: SessionInfo { state: "idle", machine: None, turbo: false, exit: None },
                run: None,
                cancel: None,
            }),
            runs: AtomicU64::new(0),
            log_tx,
            logs,
            frames: StdMutex::new(None),
            screen: StdMutex::new(None),
            card: StdMutex::new(card),
        })
    }

    pub fn log(&self, line: impl Into<String>) {
        let _ = self.log_tx.send(line.into());
    }

    pub fn info(&self) -> SessionInfo {
        self.shared.lock().unwrap().info.clone()
    }

    pub fn card(&self) -> PathBuf {
        self.card.lock().unwrap().clone()
    }

    /// Replace the card. Only while idle; the caller holds the card lock.
    pub async fn set_card(&self, path: PathBuf) -> Result<(), AppError> {
        let _ops = self.ops.lock().await;
        if self.info().state != "idle" {
            return Err(AppError::new("EBUSY", "Stop the emulator before changing the SD card."));
        }
        *self.card.lock().unwrap() = path;
        Ok(())
    }

    /// Connect a (possibly reloaded) WebView: console lines and display
    /// messages go to these channels. The current screen is sent at once.
    pub fn attach(&self, logs: Channel<Vec<String>>, frames: Channel<InvokeResponseBody>) {
        *self.logs.lock().unwrap() = Some(logs);
        if let Some(screen) = self.screen.lock().unwrap().as_ref() {
            let _ = frames.send(InvokeResponseBody::Raw(display::size_message(screen.width, screen.height)));
            let mut frame = Vec::with_capacity(24 + screen.pixels.len());
            for value in [display::MSG_FRAME, 1, 0, 0, screen.width, screen.height] {
                frame.extend_from_slice(&value.to_le_bytes());
            }
            frame.extend_from_slice(&screen.pixels);
            let _ = frames.send(InvokeResponseBody::Raw(frame));
        }
        *self.frames.lock().unwrap() = Some(frames);
    }

    fn set(&self, update: impl FnOnce(&mut SessionInfo)) {
        let info = {
            let mut shared = self.shared.lock().unwrap();
            update(&mut shared.info);
            shared.info.clone()
        };
        let _ = self.app.emit("session", info);
    }

    fn current(&self) -> Option<Arc<Run>> {
        let shared = self.shared.lock().unwrap();
        if shared.info.state == "running" { shared.run.clone() } else { None }
    }

    fn display_message(&self, message: Vec<u8>) {
        let kind = u32::from_le_bytes(message[0..4].try_into().unwrap());
        {
            let mut screen = self.screen.lock().unwrap();
            let word = |at: usize| u32::from_le_bytes(message[at..at + 4].try_into().unwrap());
            if kind == display::MSG_SIZE {
                let (width, height) = (word(4), word(8));
                *screen = Some(Framebuffer { width, height, pixels: vec![0xff; (width * height) as usize] });
            } else if let Some(screen) = screen.as_mut() {
                let mut at = 8;
                for _ in 0..word(4) {
                    let (x, y, w, h) = (word(at) as usize, word(at + 4) as usize, word(at + 8) as usize, word(at + 12) as usize);
                    at += 16;
                    for row in 0..h {
                        let start = (y + row) * screen.width as usize + x;
                        screen.pixels[start..start + w].copy_from_slice(&message[at + row * w..at + (row + 1) * w]);
                    }
                    at += w * h;
                }
            }
        }
        if let Some(path) = std::env::var_os("EINK_QEMU_SCREEN_DUMP") {
            // Tests: the whole panel as a binary PGM after every update.
            if let Some(screen) = self.screen.lock().unwrap().as_ref() {
                let mut pgm = format!("P5\n{} {}\n255\n", screen.width, screen.height).into_bytes();
                pgm.extend_from_slice(&screen.pixels);
                let temp = PathBuf::from(&path).with_extension("tmp");
                if std::fs::write(&temp, pgm).is_ok() {
                    let _ = std::fs::rename(temp, &path);
                }
            }
        }
        if let Some(frames) = self.frames.lock().unwrap().as_ref() {
            let _ = frames.send(InvokeResponseBody::Raw(message));
        }
    }

    pub async fn start(self: &Arc<Self>, options: StartOptions, flash: Vec<u8>) -> Result<SessionInfo, AppError> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        {
            let mut shared = self.shared.lock().unwrap();
            if shared.info.state != "idle" {
                return Err(AppError::busy());
            }
            shared.cancel = Some(cancel_tx);
            shared.info = SessionInfo { state: "starting", machine: None, turbo: options.turbo, exit: None };
        }
        self.set(|_| {});
        let _ops = self.ops.lock().await;
        *self.screen.lock().unwrap() = None;
        let id = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        let result = self.launch(id, &options, flash, cancel_rx).await;
        self.shared.lock().unwrap().cancel = None;
        match result {
            Ok(run) => {
                self.shared.lock().unwrap().run = Some(run);
                self.set(|info| {
                    info.state = "running";
                    info.machine = Some(options.machine.clone());
                });
                Ok(self.info())
            }
            Err((exit, error)) => {
                self.log(format!("Error: {}", error.message));
                self.set(|info| *info = SessionInfo { state: "idle", machine: None, turbo: options.turbo, exit: Some(exit) });
                Err(error)
            }
        }
    }

    fn args(&self, options: &StartOptions, flash: &Path, card: &Path, qmp: &str, display: &str) -> Result<Vec<String>, AppError> {
        let machine = options.machine.as_str();
        let mut args: Vec<String> = Vec::new();
        if let Some(share) = &self.qemu.share {
            args.extend(["-L".into(), share.to_string_lossy().into_owned()]);
        }
        args.extend([
            "-machine".into(),
            if machine == "x4pro" { format!("x4pro,panel={}", options.panel) } else { machine.to_string() },
            "-icount".into(),
            format!(
                "shift={},sleep={}",
                if machine == "picco" { 2 } else { 0 },
                if options.turbo && machine != "picco" { "off" } else { "on" }
            ),
        ]);
        if options.turbo {
            let turbo: serde_json::Map<String, Value> = serde_json::from_str(TURBO_ARGS)
                .map_err(|_| AppError::new("EQEMU", "Invalid turbo resources; run prepare-resources.sh again."))?;
            let list = turbo.get(machine).and_then(Value::as_array)
                .ok_or_else(|| AppError::new("EQEMU", "Invalid turbo resources; run prepare-resources.sh again."))?;
            args.extend(list.iter().filter_map(|arg| arg.as_str().map(String::from)));
        }
        if machine != "x4" && machine != "picco" {
            args.extend([
                "-global".into(),
                format!("{}.ghosting={}", if machine == "x3" { "uc8279" } else { "uc8179" }, if options.ghosting { "on" } else { "off" }),
            ]);
        }
        args.extend([
            "-drive".into(), format!("file={},if=mtd,format=raw", escape(&flash.to_string_lossy())),
            "-drive".into(), format!("file={},if=sd,format=raw", escape(&card.to_string_lossy())),
            "-chardev".into(), "stdio,id=cdc,mux=off".into(),
            "-serial".into(), "null".into(),
            "-global".into(), "driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc".into(),
            "-chardev".into(), format!("socket,id=qmp,{qmp}"),
            "-object".into(), "monitor-qmp,id=qmp-monitor,chardev=qmp".into(),
            "-display".into(), format!("eink,addr={}", escape(display)),
            "-nic".into(),
            if options.wifi { "user,model=esp32_wifi".into() } else { "hubport,hubid=0,model=esp32_wifi".into() },
        ]);
        Ok(args)
    }

    async fn launch(
        self: &Arc<Self>,
        id: u64,
        options: &StartOptions,
        flash: Vec<u8>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Arc<Run>, (Exit, AppError)> {
        let failed = |error: AppError| (Exit { kind: "failed", reason: Some(error.message.clone()) }, error);
        let binary = self.qemu.binary(&options.machine);
        if !binary.exists() {
            return Err(failed(AppError::new(
                "EQEMU",
                format!("QEMU is missing at {}. Build it or set EINK_QEMU_BIN.", binary.display()),
            )));
        }
        let dir = tempfile::Builder::new().prefix("eink-qemu-").tempdir().map_err(|e| failed(e.into()))?;
        let flash_path = dir.path().join("flash.bin");
        std::fs::write(&flash_path, &flash).map_err(|e| failed(e.into()))?;
        let qmp_listener = Listener::bind(dir.path(), "qmp").await.map_err(|e| failed(e.into()))?;
        let display_listener = Listener::bind(dir.path(), "display").await.map_err(|e| failed(e.into()))?;
        let card = self.card();
        let args = self
            .args(options, &flash_path, &card, &qmp_listener.chardev(), &display_listener.address())
            .map_err(failed)?;
        self.log(format!(
            "$ {} {}",
            binary.file_name().unwrap_or_default().to_string_lossy(),
            args.join(" ")
        ));

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command
            .spawn()
            .map_err(|e| failed(AppError::new("EQEMU", format!("Could not start {}: {e}", binary.display()))))?;
        for stream in [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let log = self.log_tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&line);
                            let _ = log.send(text.trim_end_matches(['\n', '\r']).to_string());
                        }
                    }
                }
            });
        }

        let (faults_tx, mut faults) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exited) = watch::channel::<Option<String>>(None);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        {
            let faults = faults_tx.clone();
            tokio::spawn(async move {
                let status = tokio::select! {
                    status = child.wait() => status,
                    _ = kill_rx => {
                        let _ = child.start_kill();
                        child.wait().await
                    }
                };
                let status = describe(status);
                let _ = exit_tx.send(Some(status.clone()));
                let _ = faults.send(format!("QEMU exited ({status})."));
            });
        }
        let kill = StdMutex::new(Some(kill_tx));

        let connect = async {
            let (qmp_stream, display_stream) = tokio::try_join!(qmp_listener.accept(), display_listener.accept())
                .map_err(|e| AppError::new("EQEMU", format!("QEMU did not connect: {e}")))?;
            let (qmp_closed_tx, qmp_closed) = oneshot::channel();
            let (events_tx, mut events) = mpsc::unbounded_channel::<Value>();
            let qmp = match qmp_stream {
                #[cfg(unix)]
                Stream::Unix(s) => Qmp::connect(s, events_tx, qmp_closed_tx).await,
                Stream::Tcp(s) => Qmp::connect(s, events_tx, qmp_closed_tx).await,
            }
            .map_err(|e| AppError::new("EQEMU", e))?;
            {
                let faults = faults_tx.clone();
                tokio::spawn(async move {
                    if let Ok(reason) = qmp_closed.await {
                        let _ = faults.send(reason);
                    }
                });
                let log = self.log_tx.clone();
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if event["event"] == "SHUTDOWN" {
                            let _ = log.send("Guest shut down.".to_string());
                        }
                    }
                });
            }
            let (sized_tx, sized) = oneshot::channel::<()>();
            {
                let session = self.clone();
                let faults = faults_tx.clone();
                tokio::spawn(async move {
                    let mut sized_tx = Some(sized_tx);
                    let sink = |message: Vec<u8>| {
                        if let Some(sized) = sized_tx.take() {
                            let _ = sized.send(());
                        }
                        session.display_message(message);
                    };
                    let result = match display_stream {
                        #[cfg(unix)]
                        Stream::Unix(s) => display::read(s, sink).await,
                        Stream::Tcp(s) => display::read(s, sink).await,
                    };
                    let _ = faults.send(match result {
                        Ok(()) => "QEMU closed the display.".to_string(),
                        Err(error) => error.to_string(),
                    });
                });
            }
            sized.await.map_err(|_| AppError::new("EQEMU", "QEMU closed the display before sending a frame."))?;
            Ok::<Qmp, AppError>(qmp)
        };

        let mut exit_watch = exited.clone();
        let outcome = tokio::select! {
            result = tokio::time::timeout(CONNECT_TIMEOUT, connect) => match result {
                Ok(result) => result.map_err(failed),
                Err(_) => Err(failed(AppError::new("EQEMU", "QEMU did not connect within 30 seconds."))),
            },
            _ = exit_watch.wait_for(|status| status.is_some()) => {
                let status = exited.borrow().clone().unwrap_or_default();
                Err(failed(AppError::new("EQEMU", format!("QEMU exited during startup ({status}). See the console for its message."))))
            }
            _ = cancel.wait_for(|cancelled| *cancelled) => {
                Err((Exit { kind: "stopped", reason: None }, AppError::new("ECANCELED", "Startup was cancelled.")))
            }
        };
        let qmp = match outcome {
            Ok(qmp) => qmp,
            Err(error) => {
                terminate(None, &kill, &exited).await;
                return Err(error);
            }
        };

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Input>();
        let run = Arc::new(Run {
            id,
            machine: options.machine.clone(),
            qmp: qmp.clone(),
            input: input_tx,
            kill,
            exited,
            stopping: AtomicBool::new(false),
            _dir: dir,
        });
        // Device input in arrival order, so a release never overtakes its press.
        {
            let qmp = qmp.clone();
            let machine = options.machine.clone();
            tokio::spawn(async move {
                let mut home = false;
                while let Some(input) = input_rx.recv().await {
                    let result = match input {
                        Input::Key(key, down) if key == "Home" => {
                            if machine != "x4pro" || home == down {
                                continue;
                            }
                            home = down;
                            qmp.execute("qom-set", Some(json!({"path": "/machine/gt911", "property": "home", "value": down}))).await
                        }
                        Input::Key(key, down) => {
                            let Some(qcode) = qcode(&key) else { continue };
                            qmp.execute("input-send-event", Some(json!({"events": [
                                {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}
                            ]}))).await
                        }
                        Input::Touch(x, y, down) => {
                            let axis = |v: f64| (v.clamp(0.0, 1.0) * 32767.0).round() as i64;
                            qmp.execute("input-send-event", Some(json!({"events": [
                                {"type": "abs", "data": {"axis": "x", "value": axis(x)}},
                                {"type": "abs", "data": {"axis": "y", "value": axis(y)}},
                                {"type": "btn", "data": {"down": down, "button": "left"}}
                            ]}))).await
                        }
                    };
                    if result.is_err() {
                        break;
                    }
                }
            });
        }
        // The first fault (QEMU exit, monitor or display loss) ends the run.
        {
            let session = self.clone();
            tokio::spawn(async move {
                if let Some(reason) = faults.recv().await {
                    session.fault(id, reason).await;
                }
            });
        }
        Ok(run)
    }

    async fn fault(self: &Arc<Self>, id: u64, reason: String) {
        let run = {
            let shared = self.shared.lock().unwrap();
            match &shared.run {
                Some(run) if run.id == id && !run.stopping.load(Ordering::SeqCst) => run.clone(),
                _ => return,
            }
        };
        self.log(reason.clone());
        let status = run.exited.borrow().clone();
        let _ops = self.ops.lock().await;
        self.finish(&run, Exit { kind: "crashed", reason: Some(status.map(|s| format!("QEMU exited ({s})")).unwrap_or(reason)) })
            .await;
    }

    /// End `run` (the caller holds `ops`). Asks QEMU to quit, then kills it.
    async fn finish(&self, run: &Arc<Run>, exit: Exit) {
        if run.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        let is_current = matches!(&self.shared.lock().unwrap().run, Some(current) if current.id == run.id);
        if !is_current {
            return;
        }
        self.set(|info| info.state = "stopping");
        terminate(Some(&run.qmp), &run.kill, &run.exited).await;
        self.shared.lock().unwrap().run = None;
        self.set(|info| {
            info.state = "idle";
            info.machine = None;
            info.exit = Some(exit);
        });
    }

    pub async fn stop(self: &Arc<Self>) -> Result<(), AppError> {
        if let Some(cancel) = self.shared.lock().unwrap().cancel.as_ref() {
            let _ = cancel.send(true);
        }
        let _ops = self.ops.lock().await;
        let run = self.shared.lock().unwrap().run.clone();
        if let Some(run) = run {
            self.finish(&run, Exit { kind: "stopped", reason: None }).await;
        }
        Ok(())
    }

    pub async fn reset(self: &Arc<Self>) -> Result<(), AppError> {
        let _ops = self.ops.lock().await;
        let run = self.current().ok_or_else(AppError::busy)?;
        if let Err(error) = self.reset_device(&run).await {
            self.log(format!("Reset failed: {error}"));
            self.finish(&run, Exit { kind: "failed", reason: Some(format!("Reset failed: {error}")) }).await;
            return Err(AppError::new("EQEMU", error));
        }
        Ok(())
    }

    async fn reset_device(&self, run: &Run) -> Result<(), String> {
        if run.machine == "x4pro" {
            let _ = run.input.send(Input::Key("Home".into(), false));
        }
        run.qmp.execute("system_reset", None).await.map(|_| ())
    }

    pub fn key(&self, key: String, down: bool) {
        if let Some(run) = self.current() {
            let _ = run.input.send(Input::Key(key, down));
        }
    }

    pub fn touch(&self, x: f64, y: f64, down: bool) {
        if let Some(run) = self.current() {
            let _ = run.input.send(Input::Touch(x, y, down));
        }
    }

    /// Run an SD card operation on the image. While the emulator runs, the
    /// guest is stopped around it; a change resets the device so the
    /// firmware mounts the card afresh (it caches FAT and directory sectors).
    pub async fn with_card<T, F>(self: &Arc<Self>, mutates: bool, op: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T, SdError> + Send + 'static,
    {
        let _ops = self.ops.lock().await;
        let card = self.card();
        let state = self.info().state;
        let blocking = |card: PathBuf| async move {
            tokio::task::spawn_blocking(move || op(&card))
                .await
                .unwrap_or_else(|_| Err(SdError::new("EIO", "The SD operation panicked.")))
        };
        match state {
            "idle" => blocking(card).await.map_err(AppError::from),
            "running" => {
                let run = self.current().ok_or_else(AppError::busy)?;
                if let Err(error) = run.qmp.execute("stop", None).await {
                    return Err(AppError::new("EQEMU", error));
                }
                let result = blocking(card).await;
                let changed = mutates && result.as_ref().err().is_none_or(|error| !error.precheck());
                let resumed = async {
                    if changed {
                        self.reset_device(&run).await?;
                        let _ = self.app.emit("sd-reset", ());
                    }
                    run.qmp.execute("cont", None).await.map(|_| ())
                }
                .await;
                if let Err(error) = resumed {
                    self.log(format!("SD operation stopped the emulator: {error}"));
                    self.finish(&run, Exit { kind: "failed", reason: Some(format!("SD operation failed: {error}")) }).await;
                    result?;
                    return Err(AppError::new("EQEMU", error));
                }
                result.map_err(AppError::from)
            }
            _ => Err(AppError::busy()),
        }
    }

    /// Stop the emulator before the app exits.
    pub async fn shutdown(self: &Arc<Self>) {
        let _ = self.stop().await;
    }
}

/// Ask QEMU to quit (when a monitor is given), then kill it if it lingers.
async fn terminate(qmp: Option<&Qmp>, kill: &StdMutex<Option<oneshot::Sender<()>>>, exited: &watch::Receiver<Option<String>>) {
    let mut exited = exited.clone();
    if exited.borrow().is_some() {
        return;
    }
    if let Some(qmp) = qmp {
        let quit = qmp.clone();
        tokio::spawn(async move {
            let _ = quit.execute("quit", None).await;
        });
        if tokio::time::timeout(QUIT_TIMEOUT, exited.wait_for(|s| s.is_some())).await.is_ok() {
            return;
        }
    }
    if let Some(kill) = kill.lock().unwrap().take() {
        let _ = kill.send(());
    }
    let _ = tokio::time::timeout(QUIT_TIMEOUT, exited.wait_for(|s| s.is_some())).await;
}

fn qcode(key: &str) -> Option<&'static str> {
    Some(match key {
        "Back" => "esc",
        "Confirm" => "ret",
        "Left" => "left",
        "Right" => "right",
        "Up" => "up",
        "Down" => "down",
        "Power" => "p",
        _ => return None,
    })
}

/// QEMU connects to sockets the app listens on, so there is no wait for a
/// socket file. Unix sockets on macOS and Linux, loopback TCP on Windows.
enum Listener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener, PathBuf),
    #[allow(dead_code)]
    Tcp(tokio::net::TcpListener, u16),
}

enum Stream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[allow(dead_code)]
    Tcp(tokio::net::TcpStream),
}

impl Listener {
    async fn bind(dir: &Path, name: &str) -> std::io::Result<Listener> {
        #[cfg(unix)]
        {
            let path = dir.join(format!("{name}.sock"));
            Ok(Listener::Unix(tokio::net::UnixListener::bind(&path)?, path))
        }
        #[cfg(not(unix))]
        {
            let _ = (dir, name);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();
            Ok(Listener::Tcp(listener, port))
        }
    }

    /// Options for `-chardev socket,id=qmp,…`.
    fn chardev(&self) -> String {
        match self {
            #[cfg(unix)]
            Listener::Unix(_, path) => format!("path={}", escape(&path.to_string_lossy())),
            Listener::Tcp(_, port) => format!("host=127.0.0.1,port={port}"),
        }
    }

    /// The eink display's `addr=`.
    fn address(&self) -> String {
        match self {
            #[cfg(unix)]
            Listener::Unix(_, path) => format!("unix:{}", path.to_string_lossy()),
            Listener::Tcp(_, port) => format!("127.0.0.1:{port}"),
        }
    }

    async fn accept(&self) -> std::io::Result<Stream> {
        match self {
            #[cfg(unix)]
            Listener::Unix(listener, _) => Ok(Stream::Unix(listener.accept().await?.0)),
            Listener::Tcp(listener, _) => {
                let (stream, _) = listener.accept().await?;
                stream.set_nodelay(true)?;
                Ok(Stream::Tcp(stream))
            }
        }
    }
}

