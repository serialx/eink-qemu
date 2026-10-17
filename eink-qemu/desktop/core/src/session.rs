//! One emulator run at a time: spawns qemu-system-*, listens for its QMP and
//! eink display connections, hands the console and the panel to its
//! `Events` (the desktop's WebView, or the command-line tool), and brackets
//! SD card edits and heap snapshots with stop / cont.

use crate::cards::Card;
use crate::display;
use crate::heap::{self, HeapError};
use crate::qmp::Qmp;
use crate::sd::SdError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

/// Where a session's output goes. The desktop forwards it to the WebView;
/// the command-line tool records it. Called from the session's tasks: keep
/// each call short and never call back into the session from it.
pub trait Events: Send + Sync + 'static {
    /// The session changed state (every change, in order).
    fn session(&self, _info: &SessionInfo) {}
    /// An SD card change reset the running guest.
    fn sd_reset(&self) {}
    /// Console lines (the firmware's log, QEMU's own output and the
    /// session's notes), batched; at most `LOG_QUEUE` wait, beyond which
    /// lines are dropped and a line says how many.
    fn logs(&self, _lines: Vec<String>) {}
    /// A display message (`display::MSG_SIZE` or `display::MSG_FRAME`), sent
    /// while the screen lock is held, so it is ordered with `with_screen`.
    fn display(&self, _message: &[u8]) {}
}

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
    pub fn no_card() -> Self {
        Self::new("ENOCARD", "No SD card is open. Open an SD image or make a new card from the card menu.")
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
    /// "stopped" (by the user), "crashed" (QEMU, its monitor or display was
    /// lost) or "failed" (could not start, or was ended by the app after an
    /// error; `reason` says which).
    pub kind: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// "idle", "starting", "running" or "stopping".
    pub state: &'static str,
    /// The board while starting, running or stopping.
    pub machine: Option<String>,
    pub turbo: bool,
    pub exit: Option<Exit>,
    /// Increases with every start, so the WebView can tell runs apart.
    pub run: u64,
    /// Increases with every change, so the WebView can ignore a snapshot
    /// older than an event it already applied.
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    pub machine: String,
    pub panel: String,
    pub wifi: bool,
    pub turbo: bool,
    pub ghosting: bool,
    /// Whether the panel animates refreshes (X3 and X4 Pro); None keeps
    /// QEMU's default, which animates.
    #[serde(default)]
    pub animate: Option<bool>,
    /// How long the board holds Power pressed at a cold boot, in guest ms
    /// (firmware that turns itself off unless Power is held); None keeps
    /// the board's default (1500).
    #[serde(default)]
    pub power_hold_ms: Option<u32>,
    /// Where the firmware log comes from: "usb" (USB Serial/JTAG, the
    /// default; ESP-IDF mirrors its console there by default) or "uart0".
    #[serde(default)]
    pub console: Option<String>,
    /// More QEMU arguments, appended as given.
    #[serde(default)]
    pub extra: Vec<String>,
}

/// Where the QEMU binaries are, the -L data directory when packaged, and
/// the plugin directory.
#[derive(Debug, Clone)]
pub struct QemuPaths {
    pub bin: PathBuf,
    pub share: Option<PathBuf>,
    pub plugins: PathBuf,
}

impl QemuPaths {
    /// QEMU's MinGW file APIs do not consistently accept Windows verbatim
    /// paths (notably qemu_find_file appends a forward slash to -L).
    /// Tauri returns those paths for packaged resources. Keep the ordinary
    /// DOS spelling whenever it names the same file; dunce preserves
    /// verbatim paths whose semantics cannot safely be represented otherwise.
    pub fn normalized(self) -> Self {
        Self {
            bin: dunce::simplified(&self.bin).to_owned(),
            share: self.share.map(|p| dunce::simplified(&p).to_owned()),
            plugins: dunce::simplified(&self.plugins).to_owned(),
        }
    }

    /// The heaptrace plugin (contrib/plugins/heaptrace.c), if this QEMU has it.
    pub fn heaptrace(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "libheaptrace.dll" } else if cfg!(target_os = "macos") { "libheaptrace.dylib" } else { "libheaptrace.so" };
        Some(self.plugins.join(name)).filter(|path| path.is_file())
    }

    pub fn binary(&self, machine: &str) -> PathBuf {
        let name = if machine == "x3" || machine == "x4" { "qemu-system-riscv32" } else { "qemu-system-xtensa" };
        self.bin.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() })
    }
}

const TURBO_ARGS: &str = include_str!(concat!(env!("OUT_DIR"), "/turbo-args.json"));
// A fresh x64 executable on Windows ARM can spend tens of seconds in the
// Windows loader before reaching main. Leave room for that cold start and
// QEMU initialization; cancellation and process exits still resolve early.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(if cfg!(windows) { 90 } else { 30 });
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_INTERVAL: Duration = Duration::from_millis(30);
/// Console lines waiting for the events sink; beyond this, lines are dropped.
pub const LOG_QUEUE: usize = 20_000;
/// Longer console lines are split.
const MAX_LINE: usize = 16 * 1024;
const RUN_PREFIX: &str = "eink-qemu-";

struct Run {
    id: u64,
    machine: String,
    qmp: Qmp,
    input: mpsc::UnboundedSender<Input>,
    /// True while an SD operation has the guest stopped: input waits.
    paused: watch::Sender<bool>,
    /// Held while one input event is sent and while pausing, so an event
    /// never reaches QEMU after it was stopped for an SD operation.
    gate: Arc<Mutex<()>>,
    kill: StdMutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<Option<String>>,
    stopping: AtomicBool,
    /// The heaptrace plugin's log, or why this run does not record allocations.
    trace: Result<PathBuf, String>,
    /// Heap pool starts found so far (see heap.rs).
    pools: Mutex<heap::Pools>,
    /// The QEMU binary and its arguments.
    command: Vec<String>,
    dir: tempfile::TempDir,
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

/// How the current run records allocations for the heap view: the plugin
/// and the ELF it reads, or why it does not.
pub type TracePlan = Result<(PathBuf, PathBuf), String>;

/// A heap snapshot, the run it is of, and that run's allocation log (or why
/// there is none).
pub struct HeapRead {
    pub run: u64,
    pub snapshot: heap::Snapshot,
    pub trace: Result<PathBuf, String>,
}

/// The panel as last drawn: 8-bit gray, row by row.
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct Session {
    events: Arc<dyn Events>,
    qemu: QemuPaths,
    /// Serialises start, stop, reset, faults and SD card operations.
    ops: Mutex<()>,
    shared: StdMutex<Shared>,
    runs: AtomicU64,
    log_tx: mpsc::Sender<String>,
    dropped: Arc<AtomicU64>,
    /// Console lines read so far, and how many of them reached `Events::logs`
    /// (or were dropped): `log_barrier` waits for the second to catch up.
    ingested: AtomicU64,
    delivered: Arc<AtomicU64>,
    log_file: Arc<StdMutex<Option<std::fs::File>>>,
    screen: StdMutex<Option<Framebuffer>>,
    card: StdMutex<Option<Card>>,
    /// Deep sleep (the VM suspended, from QMP's SUSPEND and WAKEUP events):
    /// since when, and the time asleep before that in this run.
    asleep: StdMutex<(Option<Instant>, Duration)>,
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

/// Read one line of at most `MAX_LINE` bytes (a longer line is returned in
/// pieces). Ok(false) at the end of the stream.
async fn read_line_limited<R: AsyncBufReadExt + Unpin>(reader: &mut R, line: &mut Vec<u8>) -> std::io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let room = MAX_LINE - line.len();
        let (take, done) = match available.iter().take(room).position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len().min(room), available.len() >= room),
        };
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if done || line.len() >= MAX_LINE {
            return Ok(true);
        }
    }
}

/// Where run directories go: a short path, so socket names fit the platform
/// limit (104 bytes on macOS) even when TMPDIR is long.
fn run_parent() -> PathBuf {
    if cfg!(unix) { PathBuf::from("/tmp") } else { std::env::temp_dir() }
}

/// A private directory (0700 on Unix) for one run's flash copy and sockets.
fn run_dir() -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(RUN_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let dir = builder.tempdir_in(run_parent())?;
    std::fs::write(dir.path().join("owner.pid"), std::process::id().to_string())?;
    Ok(dir)
}

/// Remove run directories left behind by an app that was killed: they
/// name a process that no longer exists.
fn scavenge_run_dirs() {
    let Ok(entries) = std::fs::read_dir(run_parent()) else { return };
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(RUN_PREFIX) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() {
            continue;
        }
        // /tmp is shared; Windows' temporary directory is the user's own.
        #[cfg(unix)]
        if std::os::unix::fs::MetadataExt::uid(&meta) != uid {
            continue;
        }
        let Some(pid) = std::fs::read_to_string(path.join("owner.pid")).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
            continue;
        };
        if !crate::host::process_alive(pid) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

impl Session {
    /// A session; call from inside a Tokio runtime, which runs its tasks.
    pub fn new(events: Arc<dyn Events>, qemu: QemuPaths, card: Option<Card>) -> Arc<Session> {
        scavenge_run_dirs();
        let (log_tx, mut log_rx) = mpsc::channel::<String>(LOG_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let delivered = Arc::new(AtomicU64::new(0));
        // EINK_QEMU_LOG: also append console lines to a file (tests).
        let log_file = Arc::new(StdMutex::new(std::env::var_os("EINK_QEMU_LOG").and_then(|path| {
            std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
        })));
        {
            let sink = events.clone();
            let dropped = dropped.clone();
            let delivered = delivered.clone();
            let file = log_file.clone();
            // Console lines are batched so a chatty firmware costs one IPC message per tick.
            tokio::spawn(async move {
                let mut batch = Vec::new();
                while log_rx.recv_many(&mut batch, 4096).await > 0 {
                    tokio::time::sleep(LOG_INTERVAL).await;
                    while batch.len() < LOG_QUEUE {
                        match log_rx.try_recv() {
                            Ok(line) => batch.push(line),
                            Err(_) => break,
                        }
                    }
                    let lost = dropped.swap(0, Ordering::Relaxed);
                    let received = batch.len() as u64;
                    if lost > 0 {
                        batch.push(format!("[eink-qemu: {lost} console lines dropped while the console was busy]"));
                    }
                    if let Some(file) = file.lock().unwrap().as_mut() {
                        use std::io::Write;
                        let _ = file.write_all((batch.join("\n") + "\n").as_bytes());
                    }
                    sink.logs(std::mem::take(&mut batch));
                    delivered.fetch_add(received + lost, Ordering::SeqCst);
                }
            });
        }
        Arc::new(Session {
            events,
            qemu: qemu.normalized(),
            ops: Mutex::new(()),
            shared: StdMutex::new(Shared {
                info: SessionInfo { state: "idle", machine: None, turbo: false, exit: None, run: 0, seq: 0 },
                run: None,
                cancel: None,
            }),
            runs: AtomicU64::new(0),
            log_tx,
            dropped,
            ingested: AtomicU64::new(0),
            delivered,
            log_file,
            screen: StdMutex::new(None),
            card: StdMutex::new(card),
            asleep: StdMutex::new((None, Duration::ZERO)),
        })
    }

    /// Whether the guest is in deep sleep: QEMU suspended the VM, and its
    /// clock stands still until a wake source (or a reset) resumes it.
    pub fn asleep(&self) -> bool {
        self.asleep.lock().unwrap().0.is_some()
    }

    /// Host time the guest has spent in deep sleep in this run.
    pub fn asleep_for(&self) -> Duration {
        let asleep = self.asleep.lock().unwrap();
        asleep.1 + asleep.0.map(|t| t.elapsed()).unwrap_or_default()
    }

    fn set_asleep(&self, now: bool) {
        let mut asleep = self.asleep.lock().unwrap();
        match (asleep.0, now) {
            (None, true) => asleep.0 = Some(Instant::now()),
            (Some(since), false) => *asleep = (None, asleep.1 + since.elapsed()),
            _ => {}
        }
    }

    pub fn log(&self, line: impl Into<String>) {
        self.ingested.fetch_add(1, Ordering::SeqCst);
        if self.log_tx.try_send(line.into()).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Wait (at most `limit`) until every console line read so far has been
    /// handed to `Events::logs`, so what follows is ordered after it.
    pub async fn log_barrier(&self, limit: Duration) {
        let target = self.ingested.load(Ordering::SeqCst);
        let started = std::time::Instant::now();
        while self.delivered.load(Ordering::SeqCst) < target && started.elapsed() < limit {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }

    /// A line the WebView already shows (its own diagnostics): only the
    /// EINK_QEMU_LOG file gets it.
    pub fn note(&self, line: &str) {
        if let Some(file) = self.log_file.lock().unwrap().as_mut() {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn qemu(&self) -> &QemuPaths {
        &self.qemu
    }

    pub fn info(&self) -> SessionInfo {
        self.shared.lock().unwrap().info.clone()
    }

    pub fn card_path(&self) -> Option<PathBuf> {
        self.card.lock().unwrap().as_ref().map(|card| card.path.clone())
    }

    /// Make `card` the card in use, after `persist` has recorded it; both
    /// happen under the operations lock, so no start slips in between.
    pub async fn select_card(&self, card: Card, persist: impl FnOnce(&Path) -> Result<(), AppError>) -> Result<(), AppError> {
        let _ops = self.ops.lock().await;
        match self.info().state {
            "idle" => {
                persist(&card.path)?;
                self.card.lock().unwrap().replace(card);
                Ok(())
            }
            "running" => {
                let run = self.current().ok_or_else(AppError::busy)?;
                {
                    let _gate = run.gate.lock().await;
                    let _ = run.paused.send(true);
                }
                let mut selected = Some(card);
                let changed: Result<(), AppError> = async {
                    let card = selected.as_ref().unwrap();
                    run.qmp.execute("stop", None).await.map_err(|e| AppError::new("EQEMU", e))?;
                    run.qmp.execute("blockdev-change-medium", Some(json!({
                        "device": "sd-card", "filename": card.path, "format": "raw"
                    }))).await.map_err(|e| AppError::new("EQEMU", e))?;
                    persist(&card.path)?;
                    self.card.lock().unwrap().replace(selected.take().unwrap());
                    self.reset_device(&run).await.map_err(|e| AppError::new("EQEMU", e))?;
                    self.events.sd_reset();
                    run.qmp.execute("cont", None).await.map_err(|e| AppError::new("EQEMU", e))?;
                    Ok(())
                }.await;
                if let Err(error) = &changed {
                    // A failed media change may have removed the old medium.
                    self.finish(&run, Exit { kind: "failed", reason: Some(format!("Changing the SD card failed: {}", error.message)) }).await;
                }
                let _ = run.paused.send(false);
                changed
            }
            _ => Err(AppError::busy()),
        }
    }

    /// Run `action` while idle with the operations lock held, so no start or
    /// SD operation overlaps it (used to create a card in place).
    pub async fn while_idle<T>(&self, action: impl FnOnce(Option<&Card>) -> Result<T, AppError>) -> Result<T, AppError> {
        let _ops = self.ops.lock().await;
        if self.info().state != "idle" {
            return Err(AppError::new("EBUSY", "Stop the emulator before changing the SD card."));
        }
        let card = self.card.lock().unwrap();
        action(card.as_ref())
    }

    /// Run `f` on the current screen with the screen lock held: no display
    /// message reaches `Events::display` while it runs, so a WebView that
    /// attaches can take the screen and its channel without a gap.
    pub fn with_screen<T>(&self, f: impl FnOnce(Option<&Framebuffer>) -> T) -> T {
        let screen = self.screen.lock().unwrap();
        f(screen.as_ref())
    }

    /// A copy of the current screen.
    pub fn screen(&self) -> Option<Framebuffer> {
        self.with_screen(|screen| screen.map(|s| Framebuffer { width: s.width, height: s.height, pixels: s.pixels.clone() }))
    }

    /// The running QEMU's command line (binary first).
    pub fn command(&self) -> Option<Vec<String>> {
        self.current().map(|run| run.command.clone())
    }

    /// The running board ("x3", "x4", "x4pro", "picco").
    pub fn machine(&self) -> Option<String> {
        self.current().map(|run| run.machine.clone())
    }

    /// The current run's heaptrace log, or why it has none.
    pub fn trace_log(&self) -> Option<Result<PathBuf, String>> {
        self.current().map(|run| run.trace.clone())
    }

    /// A QMP command on the running QEMU. Errors when idle; QEMU's own
    /// errors come back as EQMP.
    pub async fn qmp(&self, name: &str, arguments: Option<Value>) -> Result<Value, AppError> {
        let run = self.current().ok_or_else(|| AppError::new("EIDLE", "The emulator is not running."))?;
        run.qmp.execute(name, arguments).await.map_err(|e| AppError::new("EQMP", e))
    }

    fn set(&self, update: impl FnOnce(&mut SessionInfo)) {
        let info = {
            let mut shared = self.shared.lock().unwrap();
            update(&mut shared.info);
            shared.info.seq += 1;
            shared.info.clone()
        };
        self.events.session(&info);
    }

    fn current(&self) -> Option<Arc<Run>> {
        let shared = self.shared.lock().unwrap();
        if shared.info.state == "running" { shared.run.clone() } else { None }
    }

    fn display_message(&self, message: Vec<u8>) {
        let kind = u32::from_le_bytes(message[0..4].try_into().unwrap());
        let mut screen = self.screen.lock().unwrap();
        {
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
            if let Some(screen) = screen.as_ref() {
                let mut pgm = format!("P5\n{} {}\n255\n", screen.width, screen.height).into_bytes();
                pgm.extend_from_slice(&screen.pixels);
                let temp = PathBuf::from(&path).with_extension("tmp");
                if std::fs::write(&temp, pgm).is_ok() {
                    let _ = std::fs::rename(temp, &path);
                }
            }
        }
        self.events.display(&message);
        drop(screen);
    }

    pub async fn start(self: &Arc<Self>, options: StartOptions, flash: Vec<u8>, trace: TracePlan) -> Result<SessionInfo, AppError> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let id;
        {
            let mut shared = self.shared.lock().unwrap();
            if shared.info.state != "idle" {
                return Err(AppError::busy());
            }
            id = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            *self.asleep.lock().unwrap() = (None, Duration::ZERO);
            shared.cancel = Some(cancel_tx);
            shared.info = SessionInfo {
                state: "starting",
                machine: Some(options.machine.clone()),
                turbo: options.turbo,
                exit: None,
                run: id,
                seq: shared.info.seq,
            };
        }
        self.set(|_| {});
        let _ops = self.ops.lock().await;
        *self.screen.lock().unwrap() = None;
        let result = match self.card_path() {
            Some(card) => self.launch(id, &options, flash, &card, trace, cancel_rx).await,
            None => Err((Exit { kind: "failed", reason: Some(AppError::no_card().message) }, AppError::no_card())),
        };
        self.shared.lock().unwrap().cancel = None;
        match result {
            Ok(run) => {
                self.shared.lock().unwrap().run = Some(run);
                self.set(|info| info.state = "running");
                Ok(self.info())
            }
            Err((exit, error)) => {
                self.set(|info| {
                    info.state = "idle";
                    info.machine = None;
                    info.exit = Some(exit);
                });
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
            // Idle time always passes in real time (sleep=on): with native
            // speed, sleep=off would run a resting device through minutes of
            // guest time per second, into its auto-sleep, at a full core.
            // Turbo's speed-up comes from the peripheral overrides below.
            format!("shift={},sleep=on", if machine == "picco" { 2 } else { 0 }),
        ]);
        if options.turbo {
            let turbo: serde_json::Map<String, Value> = serde_json::from_str(TURBO_ARGS)
                .map_err(|_| AppError::new("EQEMU", "Invalid turbo resources; run prepare-resources.sh again."))?;
            let list = turbo.get(machine).and_then(Value::as_array)
                .ok_or_else(|| AppError::new("EQEMU", "Invalid turbo resources; run prepare-resources.sh again."))?;
            args.extend(list.iter().filter_map(|arg| arg.as_str().map(String::from)));
        }
        if machine != "x4" && machine != "picco" {
            // uc8279-68, the default X4 Pro panel, is a UC8179 subtype.
            let panel = if machine == "x3" { "uc8279" } else { "uc8179" };
            args.extend(["-global".into(), format!("{panel}.ghosting={}", if options.ghosting { "on" } else { "off" })]);
            if let Some(animate) = options.animate {
                args.extend(["-global".into(), format!("{panel}.animate={}", if animate { "on" } else { "off" })]);
            }
        }
        if let Some(ms) = options.power_hold_ms {
            let keys = if machine == "x3" || machine == "x4" { "x3-keys" } else { "x4pro-keys" };
            args.extend(["-global".into(), format!("{keys}.power-boot-ms={ms}")]);
        }
        args.extend([
            "-drive".into(), format!("file={},if=mtd,format=raw", escape(&flash.to_string_lossy())),
            "-drive".into(), format!("file={},if=sd,format=raw,id=sd-card", escape(&card.to_string_lossy())),
            "-chardev".into(), "stdio,id=cdc,mux=off".into(),
        ]);
        match options.console.as_deref().unwrap_or("usb") {
            "usb" => args.extend([
                "-serial".into(), "null".into(),
                "-global".into(), "driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc".into(),
            ]),
            "uart0" => args.extend(["-serial".into(), "chardev:cdc".into()]),
            other => return Err(AppError::new("EQEMU", format!("Unknown console {other}; use usb or uart0."))),
        }
        args.extend([
            "-chardev".into(), format!("socket,id=qmp,{qmp}"),
            "-object".into(), "monitor-qmp,id=qmp-monitor,chardev=qmp".into(),
            "-display".into(), format!("eink,addr={}", escape(display)),
            "-nic".into(),
            if options.wifi { "user,model=esp32_wifi".into() } else { "hubport,hubid=0,model=esp32_wifi".into() },
        ]);
        if !options.wifi {
            // The radio stays, with no access points to find.
            args.extend(["-global".into(), "esp32_wifi.access-points=off".into()]);
        }
        args.extend(options.extra.iter().cloned());
        Ok(args)
    }

    async fn launch(
        self: &Arc<Self>,
        id: u64,
        options: &StartOptions,
        flash: Vec<u8>,
        card: &Path,
        trace: TracePlan,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Arc<Run>, (Exit, AppError)> {
        let failed = |error: AppError| (Exit { kind: "failed", reason: Some(error.message.clone()) }, error);
        let binary = self.qemu.binary(&options.machine);
        if !binary.exists() {
            return Err(failed(AppError::new(
                "EQEMU",
                format!("QEMU is missing at {}.{}", binary.display(), if self.qemu.share.is_none() && std::env::var_os("EINK_QEMU_BIN").is_none() {
                    " Build QEMU (EINK-QEMU.md) or set EINK_QEMU_BIN."
                } else { "" }),
            )));
        }
        let dir = run_dir().map_err(|e| failed(e.into()))?;
        let flash_path = dir.path().join("flash.bin");
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new();
            file.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
            file.open(&flash_path).and_then(|mut f| f.write_all(&flash)).map_err(|e| failed(e.into()))?;
        }
        let qmp_listener = Listener::bind(dir.path(), "qmp").await.map_err(|e| failed(e.into()))?;
        let display_listener = Listener::bind(dir.path(), "display").await.map_err(|e| failed(e.into()))?;
        let mut args = self
            .args(options, &flash_path, card, &qmp_listener.chardev(), &display_listener.address())
            .map_err(failed)?;
        let trace = match trace {
            Ok((plugin, elf)) => {
                let out = dir.path().join("heaptrace.bin");
                args.extend([
                    "-plugin".into(),
                    format!("{},elf={},out={}", escape(&plugin.to_string_lossy()), escape(&elf.to_string_lossy()), escape(&out.to_string_lossy())),
                ]);
                Ok(out)
            }
            Err(reason) => Err(reason),
        };
        self.log(format!(
            "$ {} {}",
            binary.file_name().unwrap_or_default().to_string_lossy(),
            args.join(" ")
        ));
        let command_line: Vec<String> = std::iter::once(binary.to_string_lossy().into_owned()).chain(args.iter().cloned()).collect();

        let mut command = tokio::process::Command::new(&binary);
        command
            .args(&args)
            // On Windows QEMU's stdio chardev reads stdin on a thread that
            // spins on a NUL handle's endless empty reads; a pipe nobody
            // writes to blocks it instead.
            .stdin(if cfg!(windows) { std::process::Stdio::piped() } else { std::process::Stdio::null() })
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
            let session = self.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = Vec::new();
                while let Ok(true) = read_line_limited(&mut reader, &mut line).await {
                    let text = String::from_utf8_lossy(&line);
                    session.log(text.trim_end_matches(['\n', '\r']));
                }
            });
        }

        let (faults_tx, mut faults) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exited) = watch::channel::<Option<String>>(None);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        {
            let faults = faults_tx.clone();
            let stdin = child.stdin.take();
            tokio::spawn(async move {
                let _stdin = stdin;
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
                let session = self.clone();
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        match event["event"].as_str() {
                            Some("SHUTDOWN") => session.log("Guest shut down."),
                            Some("SUSPEND") => session.set_asleep(true),
                            Some("WAKEUP" | "RESET") => session.set_asleep(false),
                            _ => {}
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
                Err(_) => Err(failed(AppError::new("EQEMU", format!("QEMU did not connect within {} seconds.", CONNECT_TIMEOUT.as_secs())))),
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
        let (paused, mut paused_rx) = watch::channel(false);
        let gate = Arc::new(Mutex::new(()));
        let run = Arc::new(Run {
            id,
            machine: options.machine.clone(),
            qmp: qmp.clone(),
            input: input_tx,
            paused,
            gate: gate.clone(),
            kill,
            exited,
            stopping: AtomicBool::new(false),
            trace,
            pools: Mutex::new(heap::Pools::new()),
            command: command_line,
            dir,
        });
        // Device input in arrival order, so a release never overtakes its
        // press. While an SD operation has the guest stopped, QEMU rejects
        // input, so it waits here instead.
        {
            let qmp = qmp.clone();
            let machine = options.machine.clone();
            let session = self.clone();
            tokio::spawn(async move {
                let mut home = false;
                let mut reported = false;
                'events: while let Some(input) = input_rx.recv().await {
                    let _sending = loop {
                        let held = gate.lock().await;
                        if !*paused_rx.borrow() {
                            break held;
                        }
                        drop(held);
                        if paused_rx.wait_for(|paused| !*paused).await.is_err() {
                            break 'events;
                        }
                    };
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
                    // A rejected event is not fatal; monitor loss is a fault of its own.
                    if let Err(error) = result {
                        if !reported {
                            session.log(format!("Device input failed: {error}"));
                            reported = true;
                        }
                    }
                }
            });
        }
        // The first fault (QEMU exit, monitor or display loss) ends the run.
        // It waits for the operations lock, so a fault during startup is
        // handled once start has published the run.
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
        let _ops = self.ops.lock().await;
        let run = {
            let shared = self.shared.lock().unwrap();
            match &shared.run {
                Some(run) if run.id == id && !run.stopping.load(Ordering::SeqCst) => run.clone(),
                _ => return,
            }
        };
        self.log(reason.clone());
        // Prefer QEMU's exit status when the process is what ended.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = run.exited.borrow().clone();
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
        let _ = run.paused.send(false);
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
        let card = self.card_path().ok_or_else(AppError::no_card)?;
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
                {
                    // Wait for an event being sent, then hold new ones back.
                    let _gate = run.gate.lock().await;
                    let _ = run.paused.send(true);
                }
                if let Err(error) = run.qmp.execute("stop", None).await {
                    // The guest may or may not have stopped: end the run
                    // rather than leave it in an unknown state.
                    self.log(format!("Could not pause the emulator for the SD card: {error}"));
                    self.finish(&run, Exit { kind: "failed", reason: Some(format!("SD operation failed: {error}")) }).await;
                    return Err(AppError::new("EQEMU", error));
                }
                let result = blocking(card).await;
                if let Err(error) = &result {
                    if error.changed {
                        // The card may be inconsistent: do not let the firmware write to it.
                        let message = format!("The SD card may be damaged ({}). The emulator was stopped; check the card before starting again.", error.message);
                        self.log(message.clone());
                        self.finish(&run, Exit { kind: "failed", reason: Some(message.clone()) }).await;
                        return Err(AppError::new(error.code, message));
                    }
                }
                let changed = mutates && result.is_ok();
                let resumed = async {
                    if changed {
                        self.reset_device(&run).await?;
                        self.events.sd_reset();
                    }
                    run.qmp.execute("cont", None).await.map(|_| ())
                }
                .await;
                let _ = run.paused.send(false);
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

    /// Read the firmware's heaps (heap.rs). The guest is stopped around it,
    /// as for an SD operation; while another operation runs, EBUSY.
    /// Without an ELF the heap list is found by a scan of internal DRAM.
    pub async fn heap_snapshot(self: &Arc<Self>, elf: Option<&heap::Elf>) -> Result<HeapRead, AppError> {
        let Ok(_ops) = self.ops.try_lock() else { return Err(AppError::busy()) };
        let run = self.current().ok_or_else(|| AppError::new("EIDLE", "Start the emulator to see its heaps."))?;
        {
            let _gate = run.gate.lock().await;
            let _ = run.paused.send(true);
        }
        let mut guest = QmpGuest { qmp: &run.qmp, path: run.dir.path().join("memory.bin") };
        let lookup = match elf {
            Some(elf) => heap::Lookup::Elf(elf),
            None => heap::Lookup::Scan(heap::dram(&run.machine)),
        };
        let result = heap::snapshot(&mut guest, &lookup, &mut *run.pools.lock().await).await;
        let _ = run.paused.send(false);
        match result {
            Ok(snapshot) => Ok(HeapRead { run: run.id, snapshot, trace: run.trace.clone() }),
            Err(HeapError::Mismatch(message)) => Err(AppError::new("EMISMATCH", message)),
            Err(HeapError::Read(message)) => Err(AppError::new("EREAD", message)),
            Err(HeapError::Guest(error)) => {
                // The guest may be left stopped: end the run.
                self.log(format!("Reading the heaps stopped the emulator: {error}"));
                self.finish(&run, Exit { kind: "failed", reason: Some(format!("Heap view failed: {error}")) }).await;
                Err(AppError::new("EQEMU", error))
            }
        }
    }

    /// Stop the emulator before the app exits.
    pub async fn shutdown(self: &Arc<Self>) {
        let _ = self.stop().await;
    }
}

/// Guest memory through QMP `pmemsave` into the run directory.
struct QmpGuest<'a> {
    qmp: &'a Qmp,
    path: PathBuf,
}

impl heap::Guest for QmpGuest<'_> {
    async fn read(&mut self, addr: u32, size: u32) -> Result<Vec<u8>, String> {
        let filename = self.path.to_string_lossy();
        self.qmp.execute("pmemsave", Some(json!({"val": addr, "size": size, "filename": filename}))).await?;
        let data = tokio::fs::read(&self.path).await.map_err(|e| format!("pmemsave 0x{addr:08x}+{size}: {e}"))?;
        if data.len() != size as usize {
            return Err(format!("pmemsave 0x{addr:08x}+{size}: short read"));
        }
        Ok(data)
    }

    async fn stop(&mut self) -> Result<(), String> {
        self.qmp.execute("stop", None).await.map(|_| ())
    }

    async fn cont(&mut self) -> Result<(), String> {
        self.qmp.execute("cont", None).await.map(|_| ())
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
            // sun_path holds 104 bytes on macOS and 108 on Linux.
            if path.as_os_str().len() >= 100 {
                return Err(std::io::Error::other(format!(
                    "The socket path {} is too long for this platform.",
                    path.display()
                )));
            }
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

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn ordinary_qemu_paths_are_preserved() {
        let paths = QemuPaths {
            bin: "qemu/bin".into(), share: Some("qemu/share".into()),
            plugins: "qemu/plugins".into(),
        }.normalized();
        assert_eq!(paths.bin, Path::new("qemu/bin"));
        assert_eq!(paths.share.as_deref(), Some(Path::new("qemu/share")));
        assert_eq!(paths.plugins, Path::new("qemu/plugins"));
    }

    #[cfg(windows)]
    #[test]
    fn packaged_qemu_paths_do_not_use_verbatim_dos_paths() {
        let paths = QemuPaths {
            bin: r"\\?\C:\Users\QA User\eink-qemu\qemu\bin".into(),
            share: Some(r"\\?\C:\Users\QA User\eink-qemu\qemu\share".into()),
            plugins: r"\\?\C:\Users\QA User\eink-qemu\qemu\plugins".into(),
        }.normalized();
        assert_eq!(paths.binary("x3"), Path::new(r"C:\Users\QA User\eink-qemu\qemu\bin\qemu-system-riscv32.exe"));
        assert_eq!(paths.share.as_deref(), Some(Path::new(r"C:\Users\QA User\eink-qemu\qemu\share")));
        assert_eq!(paths.plugins, Path::new(r"C:\Users\QA User\eink-qemu\qemu\plugins"));
    }
}
