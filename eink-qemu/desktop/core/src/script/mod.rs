//! Scripted runs: steps (steps.rs) executed against a running session,
//! each producing one JSON record. The run directory collects what a run
//! observed: run.jsonl (the records), log.txt and console.jsonl (the
//! console), frames/ (every settled screen), shots/, heaps/, traces/, and
//! fail/ when a step failed.
//!
//! Times: `t` in a record is host seconds since the run started; `guest_ms`
//! is the guest's virtual clock (QEMU's guest-clock object), which under
//! -icount is the time the firmware lives in. Waits on the guest (`wait`,
//! key holds, quiet periods) count guest time; time limits count host time.

pub mod automation;
pub mod events;
pub mod image;
pub mod metrics;
pub mod readiness;
pub mod steps;

use crate::heap::BLOCK_FREE;
use crate::heapview::HeapView;
use crate::session::{AppError, Session};
use events::RunEvents;
use readiness::{Readiness, Verdict};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use steps::{Kind, Step};

/// The QOM path of the guest clock the run adds (-object guest-clock,id=…).
pub const GUEST_CLOCK_ID: &str = "eink-clock";
/// The panel device every board names "panel".
const PANEL: &str = "/machine/peripheral/panel";
const POLL: Duration = Duration::from_millis(15);
/// How long a shot right after an action waits for the screen to respond.
const ACTION_GRACE: Duration = Duration::from_secs(2);
/// The longest an input step waits for the firmware's log to go quiet.
const INPUT_QUIET_LIMIT: Duration = Duration::from_secs(3);
/// How long an action waits for the console lines read before it to arrive.
const LOG_BARRIER: Duration = Duration::from_millis(500);
/// Log lines that mean the guest crashed.
pub const CRASH_PATTERNS: &[&str] = &[
    r"Guru Meditation Error",
    r"abort\(\) was called",
    r"\*\*\*ERROR\*\*\* A stack overflow",
    r"Stack smashing protect failure",
    r"CORRUPT HEAP",
    r"assert failed:",
    r"^Backtrace: ?0x",
];

/// The guest's clock, when this QEMU has the guest-clock object.
pub struct GuestClock {
    session: Arc<Session>,
    pub enabled: bool,
}

impl GuestClock {
    pub fn new(session: Arc<Session>, enabled: bool) -> Self {
        GuestClock { session, enabled }
    }

    /// The device's time: the guest's virtual clock, plus the time it spent
    /// in deep sleep (when QEMU suspends the VM its clock stands still, but
    /// on a device time passes), so guest-time waits go on while it sleeps.
    pub async fn now_ns(&self) -> Option<i64> {
        if !self.enabled {
            return None;
        }
        let path = format!("/objects/{GUEST_CLOCK_ID}");
        let ns = self.session.qmp("qom-get", Some(json!({"path": path, "property": "ns"}))).await.ok()?.as_i64()?;
        Some(ns.saturating_add(self.session.asleep_for().as_nanos() as i64))
    }

    pub async fn now_ms(&self) -> Option<i64> {
        self.now_ns().await.map(|ns| ns / 1_000_000)
    }

    /// The raw guest clock and the part of it the CPUs spent executing
    /// (ns): between two samples, busy growth over clock growth is the CPU
    /// load. None on QEMU builds without "busy-ns", or without -icount.
    pub async fn load_sample(&self) -> Option<(i64, i64)> {
        if !self.enabled {
            return None;
        }
        let path = format!("/objects/{GUEST_CLOCK_ID}");
        let get = async |property: &str| self.session.qmp("qom-get", Some(json!({"path": path, "property": property}))).await.ok()?.as_i64();
        let ns = get("ns").await?;
        let busy = get("busy-ns").await.filter(|&b| b >= 0)?;
        Some((ns, busy))
    }
}

/// CPU load above which the firmware counts as working (it runs near 1
/// while it computes, near 0 while it idles waiting for interrupts).
const CPU_BUSY_LOAD: f64 = 0.5;

/// How long since a moment taken as (host instant, guest ns): guest time
/// when both guest readings exist, else host time.
fn since(from: (Instant, Option<i64>), guest_now: Option<i64>) -> Duration {
    match (from.1, guest_now) {
        (Some(a), Some(b)) => Duration::from_nanos((b - a).max(0) as u64),
        _ => from.0.elapsed(),
    }
}

/// A settled screen the recorder kept (frames/NNNN-<ms>ms.png).
#[derive(Debug, Clone, Serialize)]
pub struct FrameInfo {
    pub index: usize,
    /// Host seconds of the update that drew it.
    pub t: f64,
    pub guest_ms: Option<i64>,
    pub file: String,
    /// SHA-256 of the 8-bit gray pixels (not of the PNG file).
    pub pixels_sha256: String,
}

/// Keeps every settled screen as a PNG, and tells waits when the screen has
/// settled: no display update for `settle` (host time) and, where the panel
/// model reports BUSY, the panel idle for `panel_idle` of guest time since
/// the latest update. That spans the gap between a refresh and the
/// grayscale passes after it.
pub struct Recorder {
    /// The update count the screen last settled at.
    settled: AtomicU64,
    frames: Mutex<Vec<FrameInfo>>,
    stop: AtomicBool,
    /// Whether the panel reports BUSY (false on older QEMU builds).
    pub busy_known: AtomicBool,
}

impl Recorder {
    pub fn spawn(
        session: Arc<Session>,
        events: Arc<RunEvents>,
        clock: Arc<GuestClock>,
        dir: PathBuf,
        settle: Duration,
        panel_idle: Duration,
        keep: bool,
    ) -> Arc<Recorder> {
        let recorder = Arc::new(Recorder {
            settled: AtomicU64::new(0),
            frames: Mutex::default(),
            stop: AtomicBool::new(false),
            busy_known: AtomicBool::new(true),
        });
        let me = recorder.clone();
        tokio::spawn(async move {
            let mut seen = 0u64;
            // The latest display update: its count, host seconds and guest ms.
            let mut change: (u64, f64, Option<i64>) = (0, 0.0, None);
            // When the panel was first seen idle after the latest update.
            let mut idle_from: Option<(Instant, Option<i64>)> = None;
            let mut last_sha = String::new();
            while !me.stop.load(Ordering::Relaxed) {
                tokio::time::sleep(POLL).await;
                let screen = events.screen();
                if screen.updates == seen {
                    continue;
                }
                if screen.updates != change.0 {
                    change = (screen.updates, events.now(), clock.now_ms().await);
                    idle_from = None;
                }
                let mut guest_now = None;
                // Asleep, the panel cannot finish a refresh: do not wait on BUSY.
                if me.busy_known.load(Ordering::Relaxed) && !session.asleep() {
                    match session.qmp("qom-get", Some(json!({"path": PANEL, "property": "busy"}))).await {
                        Ok(Value::Bool(true)) => {
                            idle_from = None;
                            continue;
                        }
                        Ok(Value::Bool(false)) => {
                            guest_now = clock.now_ns().await;
                            idle_from.get_or_insert((Instant::now(), guest_now));
                        }
                        Ok(_) | Err(_) if session.machine().is_some() => me.busy_known.store(false, Ordering::Relaxed),
                        _ => continue,
                    }
                }
                if screen.last_update.is_some_and(|t| t.elapsed() < settle) {
                    continue;
                }
                if me.busy_known.load(Ordering::Relaxed) {
                    if let Some(from) = idle_from {
                        if since(from, guest_now) < panel_idle {
                            continue;
                        }
                    }
                }
                // Nothing new arrived while the panel was asked about.
                if events.screen().updates != screen.updates {
                    continue;
                }
                let (_, t, guest) = change;
                if keep {
                    if let Some(fb) = session.screen() {
                        let sha = crate::flash::sha256_hex(&fb.pixels);
                        if sha != last_sha {
                            let mut frames = me.frames.lock().unwrap();
                            let index = frames.len() + 1;
                            let file = match guest {
                                Some(ms) => format!("frames/{index:04}-{ms}ms.png"),
                                None => format!("frames/{index:04}-h{}ms.png", (t * 1000.0) as u64),
                            };
                            if image::write_png(&dir.join(&file), fb.width, fb.height, &fb.pixels).is_ok() {
                                frames.push(FrameInfo { index, t: round3(t), guest_ms: guest, file, pixels_sha256: sha.clone() });
                                last_sha = sha;
                            }
                        }
                    }
                }
                seen = screen.updates;
                idle_from = None;
                me.settled.store(seen, Ordering::SeqCst);
            }
        });
        recorder
    }

    pub fn settled(&self) -> u64 {
        self.settled.load(Ordering::SeqCst)
    }

    pub fn frames(&self) -> Vec<FrameInfo> {
        self.frames.lock().unwrap().clone()
    }

    pub fn last_frame(&self) -> Option<FrameInfo> {
        self.frames.lock().unwrap().last().cloned()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub struct RunOptions {
    /// The run directory (created by the caller).
    pub dir: PathBuf,
    /// The default time limit of waits, in host seconds.
    pub timeout: f64,
    /// The time limit of `ready` without one, in host seconds.
    pub ready_timeout: f64,
    /// How long the display must be unchanged to count as settled.
    pub settle: Duration,
    /// How long the panel must have been idle (not BUSY), in guest time.
    pub panel_idle: Duration,
    /// How long the log must have been quiet before an input step, in guest
    /// time (zero: do not wait).
    pub input_quiet: Duration,
    /// Quiet screen seconds that make firmware without CrossPoint's log ready.
    pub ready_quiet: f64,
    /// The firmware's ELF, for heap sites and allocation traces.
    pub elf: Option<PathBuf>,
    /// Log lines that fail the run as a guest crash.
    pub crash_patterns: Vec<Regex>,
    /// Keep every settled screen in frames/.
    pub frames: bool,
    /// Whether the log carries the ROM's boot banner (the USB console does;
    /// the emulated boards' ROM prints nothing on UART0). Without it a boot
    /// after `reset` shows itself by drawing.
    pub rom_banner: bool,
}

impl RunOptions {
    pub fn new(dir: PathBuf) -> Self {
        RunOptions {
            dir,
            timeout: 60.0,
            ready_timeout: 180.0,
            settle: Duration::from_millis(200),
            panel_idle: Duration::from_millis(300),
            input_quiet: Duration::from_millis(200),
            ready_quiet: 5.0,
            elf: None,
            crash_patterns: CRASH_PATTERNS.iter().map(|p| Regex::new(p).unwrap()).collect(),
            frames: true,
            rom_banner: true,
        }
    }
}

/// A failed step: why, and what was found before it failed.
struct Failure {
    message: String,
    fields: Map<String, Value>,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Failure { message: message.into(), fields: Map::new() }
    }
}

impl From<AppError> for Failure {
    fn from(e: AppError) -> Self {
        Failure::new(e.message)
    }
}

type Fields = Map<String, Value>;
type StepResult = Result<Fields, Failure>;

/// Where records go besides run.jsonl (the CLI prints them).
pub type Sink = Box<dyn FnMut(&Value) + Send>;

pub struct Runner {
    /// Trace windows started so far (validate takes them into account).
    pub trace_names: Vec<String>,
    pub session: Arc<Session>,
    pub events: Arc<RunEvents>,
    pub clock: Arc<GuestClock>,
    pub frames: Arc<Recorder>,
    opts: RunOptions,
    timeout: f64,
    /// wait-log looks at lines from here on: the last action, ready, or the
    /// line after the last match.
    log_cursor: usize,
    /// wait-screen needs a settled update after this count.
    screen_cursor: u64,
    /// An action happened and no wait followed it yet: a shot first waits
    /// for the screen to respond.
    pending_action: bool,
    readiness: Readiness,
    readiness_pos: usize,
    /// Screen updates before the current boot (a reset): readiness by a
    /// quiet screen needs one after it.
    readiness_screen_base: u64,
    /// Host seconds the current boot started: the run's start, or a reset.
    boot_started: f64,
    /// The last heap record's numbers, for the next one's changes.
    last_heap: Option<(String, Fields)>,
    crash_pos: usize,
    heap: HeapView,
    traces: HashMap<String, u64>,
    shots: HashMap<String, PathBuf>,
    /// Output names taken so far, as "kind/name".
    names: HashSet<String>,
    /// Records so far; the next record's `i`.
    pub index: usize,
    /// The last step record.
    pub last_step: Option<Value>,
    records: BufWriter<File>,
    sink: Sink,
    /// The first failed record, if any.
    pub failed: Option<Value>,
}

fn epoch_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

/// A name with `{i}` replaced by the innermost repeat's iteration.
fn expand(name: &str, iters: &[u32]) -> String {
    name.replace("{i}", &iters.last().copied().unwrap_or(1).to_string())
}

impl Runner {
    pub fn new(session: Arc<Session>, events: Arc<RunEvents>, guest_clock: bool, opts: RunOptions, sink: Sink) -> Result<Runner, String> {
        for sub in ["frames", "shots", "heaps", "traces"] {
            std::fs::create_dir_all(opts.dir.join(sub)).map_err(|e| format!("cannot create {}: {e}", opts.dir.join(sub).display()))?;
        }
        let records = File::create(opts.dir.join("run.jsonl")).map(BufWriter::new).map_err(|e| e.to_string())?;
        let clock = Arc::new(GuestClock::new(session.clone(), guest_clock));
        let frames = Recorder::spawn(session.clone(), events.clone(), clock.clone(), opts.dir.clone(), opts.settle, opts.panel_idle, opts.frames);
        Ok(Runner {
            trace_names: Vec::new(),
            timeout: opts.timeout,
            session,
            events,
            clock,
            frames,
            opts,
            log_cursor: 0,
            screen_cursor: 0,
            pending_action: false,
            readiness: Readiness::new(),
            readiness_pos: 0,
            readiness_screen_base: 0,
            boot_started: 0.0,
            last_heap: None,
            crash_pos: 0,
            heap: HeapView::default(),
            traces: HashMap::new(),
            shots: HashMap::new(),
            names: HashSet::new(),
            index: 0,
            last_step: None,
            records,
            sink,
            failed: None,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.opts.dir
    }

    /// Write a record to run.jsonl and the sink.
    pub fn emit(&mut self, record: &Value) {
        let _ = writeln!(self.records, "{record}");
        let _ = self.records.flush();
        (self.sink)(record);
    }

    /// The latest settled screen kept in frames/, if any.
    pub fn latest_frame(&self) -> Option<FrameInfo> {
        self.frames.last_frame()
    }

    /// Run `steps` in order; false at the first failure (recorded, with the
    /// failure bundle in fail/). After the last step the log is checked once
    /// more for a crash.
    pub async fn run(&mut self, steps: &[Step]) -> bool {
        if let Some(elf) = self.opts.elf.clone() {
            let _ = self.heap.elf(&elf).await;
        }
        if !self.run_list(steps, Vec::new()).await {
            return false;
        }
        // A crash printed with or just after the last step's success.
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.session.log_barrier(LOG_BARRIER).await;
        if let Err(failure) = self.check_alive() {
            self.record_failure("check", "(log check after the last step)", &[], Instant::now(), failure).await;
            return false;
        }
        true
    }

    /// Record that steps were cut short (a session stopped or interrupted
    /// while they ran): a failed record, so the run does not read as passed.
    pub async fn record_cancelled(&mut self, reason: &str) {
        self.record_failure("cancelled", "(steps cancelled)", &[], Instant::now(), Failure::new(reason)).await;
    }

    fn run_list<'a>(&'a mut self, steps: &'a [Step], iters: Vec<u32>) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            for step in steps {
                if let Kind::Repeat { count, body } = &step.kind {
                    for n in 1..=*count {
                        let mut inner = iters.clone();
                        inner.push(n);
                        if !self.run_list(body, inner).await {
                            return false;
                        }
                    }
                    continue;
                }
                if !self.run_step(step, &iters).await {
                    return false;
                }
            }
            true
        })
    }

    fn base_record(&self, kind: &str, text: &str, iters: &[u32], started: Instant, guest: Option<i64>, ok: bool) -> Map<String, Value> {
        let mut record = Map::new();
        record.insert("type".into(), json!("step"));
        record.insert("i".into(), json!(self.index));
        record.insert("step".into(), json!(kind));
        record.insert("text".into(), json!(text));
        if !iters.is_empty() {
            record.insert("iter".into(), json!(iters));
        }
        record.insert("ok".into(), json!(ok));
        record.insert("t".into(), json!(round3(self.events.now())));
        record.insert("ms".into(), json!(started.elapsed().as_millis() as u64));
        record.insert("guest_ms".into(), json!(guest));
        record
    }

    async fn record_failure(&mut self, kind: &str, text: &str, iters: &[u32], started: Instant, failure: Failure) {
        let guest = self.clock.now_ms().await;
        let mut record = self.base_record(kind, text, iters, started, guest, false);
        for (k, v) in failure.fields {
            record.insert(k, v);
        }
        record.insert("error".into(), json!(failure.message));
        self.index += 1;
        let mut record = Value::Object(record);
        let bundle = self.failure_bundle(&record).await;
        record["fail_dir"] = json!(bundle);
        self.emit(&record);
        self.last_step = Some(record.clone());
        self.failed.get_or_insert(record);
    }

    async fn run_step(&mut self, step: &Step, iters: &[u32]) -> bool {
        let started = Instant::now();
        let result = self.exec(step, iters).await;
        match result {
            Ok(fields) => {
                let guest = self.clock.now_ms().await;
                let mut record = self.base_record(step.kind.name(), &step.text, iters, started, guest, true);
                for (k, v) in fields {
                    record.insert(k, v);
                }
                self.index += 1;
                let record = Value::Object(record);
                self.emit(&record);
                self.last_step = Some(record);
                true
            }
            Err(failure) => {
                self.record_failure(step.kind.name(), &step.text, iters, started, failure).await;
                false
            }
        }
    }

    /// A unique name for a step's output of one kind (shots, heaps,
    /// traces): a name taken before gets -2, -3, …, skipping names taken
    /// explicitly.
    fn unique(&mut self, kind: &str, name: &str) -> String {
        let mut candidate = name.to_string();
        let mut n = 1;
        while self.names.contains(&format!("{kind}/{candidate}")) {
            n += 1;
            candidate = format!("{name}-{n}");
        }
        self.names.insert(format!("{kind}/{candidate}"));
        candidate
    }

    /// Fail if the guest crashed or QEMU is gone.
    fn check_alive(&mut self) -> Result<(), Failure> {
        if let Some(info) = self.events.state() {
            if info.state != "running" && info.state != "starting" {
                let reason = info.exit.as_ref().and_then(|e| e.reason.clone()).unwrap_or_else(|| format!("state {}", info.state));
                return Err(Failure::new(format!("the emulator is no longer running: {reason}")));
            }
        }
        let patterns = &self.opts.crash_patterns;
        let (found, end) = self.events.with_lines_end(self.crash_pos, |lines| {
            lines.iter().find(|l| patterns.iter().any(|p| p.is_match(&l.text))).map(|l| l.text.clone())
        });
        self.crash_pos = end;
        if let Some(line) = found {
            let mut f = Failure::new(format!("the guest crashed: {line}"));
            f.fields.insert("crash_line".into(), json!(line));
            return Err(f);
        }
        Ok(())
    }

    /// Mark an action: later waits look only at what follows. Lines read
    /// before it are delivered first, so none of them can satisfy a wait.
    async fn action(&mut self) {
        self.session.log_barrier(LOG_BARRIER).await;
        self.log_cursor = self.events.line_count();
        self.screen_cursor = self.events.screen().updates;
        self.pending_action = true;
    }

    /// Before input: wait until the firmware's log has been quiet and the
    /// panel and the CPU idle for `input_quiet` of guest time, at most INPUT_QUIET_LIMIT.
    /// Firmware busy with background work (laying out pages) can miss a
    /// short press.
    async fn input_quiet(&mut self, f: &mut Fields) -> Result<(), Failure> {
        if self.opts.input_quiet.is_zero() {
            return Ok(());
        }
        let started = Instant::now();
        let mut count = self.events.line_count();
        let mut quiet_from = (Instant::now(), self.clock.now_ns().await);
        let mut load = self.clock.load_sample().await;
        loop {
            self.check_alive()?;
            let guest = self.clock.now_ns().await;
            let now_count = self.events.line_count();
            let busy = self.panel_busy().await == Some(true);
            // Firmware working without logging (background layout, a
            // prewarm) can miss a press as surely as one that logs.
            let sample = self.clock.load_sample().await;
            let computing = match (load, sample) {
                (Some((n0, b0)), Some((n1, b1))) if n1 > n0 => (b1 - b0) as f64 / (n1 - n0) as f64 > CPU_BUSY_LOAD,
                _ => false,
            };
            load = sample.or(load);
            if now_count != count || busy || computing {
                count = now_count;
                quiet_from = (Instant::now(), guest);
            } else if since(quiet_from, guest) >= self.opts.input_quiet {
                break;
            }
            if started.elapsed() > INPUT_QUIET_LIMIT {
                f.insert("input_quiet_reached".into(), json!(false));
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        f.insert("waited_ms".into(), json!(started.elapsed().as_millis() as u64));
        Ok(())
    }

    async fn panel_busy(&mut self) -> Option<bool> {
        // Asleep, the panel cannot finish a refresh; a press may wake the device.
        if !self.frames.busy_known.load(Ordering::Relaxed) || self.session.asleep() {
            return None;
        }
        self.session.qmp("qom-get", Some(json!({"path": PANEL, "property": "busy"}))).await.ok()?.as_bool()
    }

    /// Firmware-neutral counters so far: display updates the guest sent,
    /// and the panel model's completed refreshes and their guest time (null
    /// on QEMU builds without them).
    pub async fn counters(&mut self) -> Value {
        let prop = async |name: &str| self.session.qmp("qom-get", Some(json!({"path": PANEL, "property": name}))).await.ok().and_then(|v| v.as_u64());
        let refreshes = prop("refreshes").await;
        let refresh_ns = prop("refresh-ns").await;
        json!({
            "screen_updates": self.events.screen().updates,
            "panel_refreshes": refreshes,
            "panel_refresh_ms": refresh_ns.map(|ns| (ns as f64 / 1e5).round() / 10.0),
        })
    }

    /// Whether the device is in deep sleep: QEMU suspended it, or its RTC
    /// says so (deep sleep with a timer armed keeps the VM running).
    async fn deep_sleep(&mut self) -> bool {
        if self.session.asleep() {
            return true;
        }
        let path = match self.session.machine().as_deref() {
            Some("x3" | "x4") => "/machine/rtccntl",
            Some(_) => "/machine/soc/rtc_cntl",
            None => return false,
        };
        let reply = self.session.qmp("qom-get", Some(json!({"path": path, "property": "deep-sleep"}))).await;
        reply.ok().and_then(|v| v.as_bool()).unwrap_or(false)
    }

    /// A new boot is starting (reset): readiness waits for it.
    async fn begin_reset(&mut self) {
        self.session.log_barrier(LOG_BARRIER).await;
        self.readiness = Readiness::after_reset(&self.readiness);
        if !self.opts.rom_banner {
            self.readiness.skip_rom_wait();
        }
        self.readiness_pos = self.events.line_count();
        self.readiness_screen_base = self.events.screen().updates;
        self.boot_started = self.events.now();
    }

    /// Let the guest run for `ms` of its own time (host time without the
    /// guest clock), at most ten times as long plus five seconds of host time.
    async fn guest_sleep(&mut self, ms: f64) -> Result<(), Failure> {
        let ms = ms.clamp(0.0, steps::MAX_SECONDS * 1000.0);
        let host_limit = Duration::from_secs_f64(ms / 1000.0 * 10.0 + 5.0);
        let started = Instant::now();
        match self.clock.now_ns().await {
            Some(start) => {
                let target = start.saturating_add((ms * 1_000_000.0) as i64);
                loop {
                    self.check_alive()?;
                    let now = self.clock.now_ns().await.unwrap_or(target);
                    if now >= target {
                        return Ok(());
                    }
                    if started.elapsed() > host_limit {
                        return Err(Failure::new(format!("the guest clock did not advance {ms} ms within {:.0} s", host_limit.as_secs_f64())));
                    }
                    let left = Duration::from_nanos((target - now) as u64);
                    tokio::time::sleep(left.min(POLL)).await;
                }
            }
            None => {
                let end = started + Duration::from_secs_f64(ms / 1000.0);
                while Instant::now() < end {
                    self.check_alive()?;
                    tokio::time::sleep((end - Instant::now()).min(POLL)).await;
                }
                Ok(())
            }
        }
    }

    /// The last few log lines, for an error message.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let count = self.events.line_count();
        self.events.with_lines(count.saturating_sub(n), |lines| lines.iter().map(|l| l.text.clone()).collect())
    }

    fn machine(&self) -> String {
        self.session.machine().unwrap_or_default()
    }

    async fn exec(&mut self, step: &Step, iters: &[u32]) -> StepResult {
        self.check_alive()?;
        if matches!(step.kind, Kind::Ready { .. } | Kind::Wait { .. } | Kind::WaitLog { .. } | Kind::WaitScreen { .. } | Kind::WaitQuiet { .. }) {
            self.pending_action = false;
        }
        let mut f = Fields::new();
        match &step.kind {
            Kind::Ready { timeout } => {
                let limit = timeout.unwrap_or(self.opts.ready_timeout);
                let started = Instant::now();
                loop {
                    let readiness = &mut self.readiness;
                    let ((), end) = self.events.with_lines_end(self.readiness_pos, |lines| {
                        for l in lines {
                            readiness.feed(&l.text, l.t);
                        }
                    });
                    self.readiness_pos = end;
                    let now = self.events.now();
                    let asleep = self.deep_sleep().await;
                    let screen = self.events.screen();
                    // A blank panel (all one gray: before the firmware draws,
                    // or mid-flash) is not a boot that finished drawing.
                    let drawn = self.session.screen().is_some_and(|fb| fb.pixels.first().is_some_and(|&p| fb.pixels.iter().any(|&q| q != p)));
                    // A device in deep sleep is quiet because it is off, not ready.
                    let quiet = (drawn && !asleep && screen.updates > self.readiness_screen_base)
                        .then(|| screen.last_update.map(|t| t.elapsed().as_secs_f64()))
                        .flatten();
                    // A device in deep sleep is not ready, whatever its last boot
                    // decided: it is ready again after its next boot.
                    let verdict = match self.readiness.verdict(quiet, self.opts.ready_quiet, now) {
                        Verdict::Ready(_) if asleep => Verdict::Waiting,
                        verdict => verdict,
                    };
                    match verdict {
                        Verdict::Ready(how) => {
                            self.check_alive()?;
                            f.insert("how".into(), json!(how));
                            // From this boot's start (its ROM banner; without one, the
                            // run's start or the last reset) to Home for CrossPoint, or
                            // to the last screen update before the quiet.
                            let start = self.readiness.rom_banner_at().map_or(self.boot_started, |t| t.max(self.boot_started));
                            let drawn = screen.last_update.map(|u| now - u.elapsed().as_secs_f64()).unwrap_or(now);
                            f.insert("boot_s".into(), json!(round3(self.readiness.home_at.unwrap_or(drawn) - start)));
                            if let Some(ms) = self.readiness.home_fw_ms {
                                f.insert("home_fw_ms".into(), json!(ms));
                            }
                            f.insert("restarts".into(), json!(self.readiness.restarts));
                            self.log_cursor = end;
                            self.screen_cursor = screen.updates;
                            return Ok(f);
                        }
                        Verdict::Failed(why) => {
                            let mut failure = Failure::new(why);
                            failure.fields.insert("last_lines".into(), json!(self.tail(8)));
                            return Err(failure);
                        }
                        Verdict::Waiting => {}
                    }
                    self.check_alive()?;
                    if started.elapsed().as_secs_f64() > limit {
                        let why = if asleep && !self.readiness.power_wait {
                            "the device is in deep sleep; a key that wakes it (press:power) starts a new boot".to_string()
                        } else {
                            self.readiness.waiting_for()
                        };
                        let mut failure = Failure::new(format!("not ready after {limit} s: {why}"));
                        failure.fields.insert("last_lines".into(), json!(self.tail(8)));
                        return Err(failure);
                    }
                    tokio::time::sleep(POLL).await;
                }
            }
            Kind::Wait { seconds } => {
                self.guest_sleep(seconds * 1000.0).await?;
            }
            Kind::WaitLog { pattern } => {
                let started = Instant::now();
                loop {
                    let from = self.log_cursor;
                    let (found, _) = self.events.with_lines_end(from, |lines| {
                        lines.iter().position(|l| pattern.is_match(&l.text)).map(|i| (i, lines[i].text.clone(), lines[i].t))
                    });
                    if let Some((i, line, t)) = found {
                        self.check_alive()?;
                        self.log_cursor = from + i + 1;
                        f.insert("line".into(), json!(line));
                        f.insert("line_t".into(), json!(round3(t)));
                        if let Some(ms) = metrics::firmware_ms(&line) {
                            f.insert("fw_ms".into(), json!(ms));
                        }
                        let values = metrics::line_values(&line);
                        if !values.is_empty() {
                            f.insert("values".into(), Value::Object(values));
                        }
                        return Ok(f);
                    }
                    self.check_alive()?;
                    if started.elapsed().as_secs_f64() > self.timeout {
                        let mut failure = Failure::new(format!(
                            "no log line matched /{}/ within {} s after the last action (press, touch, swipe, reset or ready)",
                            pattern.as_str(),
                            self.timeout
                        ));
                        failure.fields.insert("last_lines".into(), json!(self.tail(8)));
                        return Err(failure);
                    }
                    tokio::time::sleep(POLL).await;
                }
            }
            Kind::WaitScreen { timeout } => {
                let limit = timeout.unwrap_or(self.timeout);
                let started = Instant::now();
                let before = self.session.screen().map(|s| crate::flash::sha256_hex(&s.pixels));
                loop {
                    let settled = self.frames.settled();
                    if settled > self.screen_cursor && settled == self.events.screen().updates {
                        self.check_alive()?;
                        self.screen_cursor = settled;
                        let after = self.session.screen().map(|s| crate::flash::sha256_hex(&s.pixels));
                        f.insert("changed".into(), json!(before != after));
                        if let Some(frame) = self.frames.last_frame() {
                            f.insert("frame".into(), json!(frame.file));
                        }
                        return Ok(f);
                    }
                    self.check_alive()?;
                    if started.elapsed().as_secs_f64() > limit {
                        return Err(Failure::new(format!("the screen did not change within {limit} s after the last action")));
                    }
                    tokio::time::sleep(POLL).await;
                }
            }
            Kind::WaitQuiet { seconds } => {
                let need = Duration::from_secs_f64(*seconds);
                let limit = self.timeout.max(seconds * 3.0 + 5.0);
                let started = Instant::now();
                let mut count = self.events.screen().updates;
                let mut quiet_from = (Instant::now(), self.clock.now_ns().await);
                loop {
                    self.check_alive()?;
                    let guest = self.clock.now_ns().await;
                    let screen = self.events.screen();
                    if screen.updates != count {
                        count = screen.updates;
                        quiet_from = (Instant::now(), guest);
                    } else if since(quiet_from, guest) >= need && self.frames.settled() == screen.updates {
                        self.screen_cursor = screen.updates;
                        if let Some(frame) = self.frames.last_frame() {
                            f.insert("frame".into(), json!(frame.file));
                        }
                        return Ok(f);
                    }
                    if started.elapsed().as_secs_f64() > limit {
                        return Err(Failure::new(format!("the screen did not stay unchanged for {seconds} s within {limit:.0} s")));
                    }
                    tokio::time::sleep(POLL).await;
                }
            }
            Kind::Timeout { seconds } => {
                self.timeout = *seconds;
                f.insert("timeout_s".into(), json!(seconds));
            }
            Kind::Press { key, ms } => {
                let machine = self.machine();
                let (path, property) = key_path(&machine, key).map_err(Failure::new)?;
                self.input_quiet(&mut f).await?;
                self.action().await;
                self.session.qmp("qom-set", Some(json!({"path": path, "property": property, "value": true}))).await?;
                let held = self.guest_sleep(f64::from(*ms)).await;
                self.session.qmp("qom-set", Some(json!({"path": path, "property": property, "value": false}))).await?;
                held?;
                f.insert("key".into(), json!(key));
                f.insert("hold_ms".into(), json!(ms));
            }
            Kind::Touch { x, y, ms } => {
                let (w, h) = self.touch_panel()?;
                let (px, py) = (x.resolve(w), y.resolve(h));
                self.input_quiet(&mut f).await?;
                self.action().await;
                self.pointer(px, py, w, h, Some(true)).await?;
                let held = self.guest_sleep(f64::from(*ms)).await;
                self.pointer(px, py, w, h, Some(false)).await?;
                held?;
                f.insert("x".into(), json!(px.round()));
                f.insert("y".into(), json!(py.round()));
            }
            Kind::Swipe { from, to, ms } => {
                let (w, h) = self.touch_panel()?;
                let (x0, y0, x1, y1) = (from.0.resolve(w), from.1.resolve(h), to.0.resolve(w), to.1.resolve(h));
                self.input_quiet(&mut f).await?;
                self.action().await;
                self.pointer(x0, y0, w, h, Some(true)).await?;
                let moves = 12;
                let mut result = Ok(());
                for i in 1..=moves {
                    let k = f64::from(i) / f64::from(moves);
                    result = self.guest_sleep(f64::from(*ms) / f64::from(moves)).await;
                    if result.is_err() {
                        break;
                    }
                    self.pointer(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k, w, h, None).await?;
                }
                self.pointer(x1, y1, w, h, Some(false)).await?;
                result?;
                f.insert("from".into(), json!([x0.round(), y0.round()]));
                f.insert("to".into(), json!([x1.round(), y1.round()]));
            }
            Kind::Reset => {
                self.begin_reset().await;
                self.session.reset().await?;
                self.action().await;
            }
            Kind::Shot { name } => {
                if self.pending_action {
                    // Right after an action: give the firmware time to start drawing.
                    let waited = Instant::now();
                    while self.events.screen().updates <= self.screen_cursor && waited.elapsed() < ACTION_GRACE {
                        self.check_alive()?;
                        tokio::time::sleep(POLL).await;
                    }
                    self.pending_action = false;
                    f.insert("changed_after_action".into(), json!(self.events.screen().updates > self.screen_cursor));
                }
                let fb = self.settled_screen().await?;
                let name = self.unique("shots", &expand(name, iters));
                let file = format!("shots/{name}.png");
                let path = self.opts.dir.join(&file);
                image::write_png(&path, fb.width, fb.height, &fb.pixels).map_err(Failure::new)?;
                self.shots.insert(name.clone(), path);
                f.insert("name".into(), json!(name));
                f.insert("file".into(), json!(file));
                f.insert("pixels_sha256".into(), json!(crate::flash::sha256_hex(&fb.pixels)));
                f.insert("size".into(), json!([fb.width, fb.height]));
            }
            Kind::Heap { name } => {
                let label = expand(name.as_deref().unwrap_or("heap"), iters);
                let label = self.unique("heaps", &label);
                let snap = self.read_heap().await?;
                let file = format!("heaps/{label}.json");
                std::fs::write(self.opts.dir.join(&file), snap.to_string()).map_err(|e| Failure::new(e.to_string()))?;
                f.insert("name".into(), json!(label));
                f.insert("file".into(), json!(file));
                let summary = heap_summary(&snap);
                if let Some((previous, before)) = &self.last_heap {
                    let mut change = Map::new();
                    change.insert("heap".into(), json!(previous));
                    for key in ["free", "largest", "min_free", "dram_free", "dram_largest", "internal_free"] {
                        if let (Some(a), Some(b)) = (before.get(key).and_then(Value::as_i64), summary.get(key).and_then(Value::as_i64)) {
                            change.insert(key.into(), json!(b - a));
                        }
                    }
                    f.insert("since_previous".into(), Value::Object(change));
                }
                self.last_heap = Some((label.clone(), summary.clone()));
                for (k, v) in summary {
                    f.insert(k, v);
                }
            }
            Kind::TraceStart { name } => {
                self.trace_log()?;
                self.traces.insert(name.clone(), epoch_us());
                f.insert("name".into(), json!(name));
            }
            Kind::TraceStop { name } => {
                let from = *self.traces.get(name).ok_or_else(|| Failure::new(format!("no trace-start:{name} before this step")))?;
                self.trace_log()?;
                // Let the plugin's buffered writes reach the file.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let to = epoch_us();
                let window = self.heap.trace_window(from, to).ok_or_else(|| Failure::new("the allocation log could not be read"))?;
                let label = self.unique("traces", &expand(name, iters));
                let file = format!("traces/{label}.json");
                std::fs::write(self.opts.dir.join(&file), window.to_string()).map_err(|e| Failure::new(e.to_string()))?;
                f.insert("name".into(), json!(label));
                f.insert("file".into(), json!(file));
                f.insert("window_ms".into(), json!((to - from) / 1000));
                for key in ["allocs", "alloc_bytes", "freed", "kept", "kept_bytes", "older_freed", "older_freed_bytes"] {
                    f.insert(key.into(), window[key].clone());
                }
                let top: Vec<Value> = window["sites"].as_array().map(|s| s.iter().take(10).map(|w| json!({
                    "fn": w["fn"], "allocs": w["allocs"], "kept_bytes": w["kept_bytes"], "alloc_bytes": w["alloc_bytes"],
                })).collect()).unwrap_or_default();
                f.insert("top_sites".into(), json!(top));
            }
            Kind::Mark { name } => {
                // A mark used again (in a repeat without {i}) gets -2, -3, ... so
                // each of its windows stays apart in a comparison.
                let label = self.unique("marks", &expand(name, iters));
                f.insert("name".into(), json!(label));
                // Counters at the mark, so a comparison can measure the window after it.
                if let Value::Object(counters) = self.counters().await {
                    f.extend(counters);
                }
            }
            Kind::ExpectLog { pattern } => {
                self.session.log_barrier(LOG_BARRIER).await;
                let found = self.events.with_lines(0, |lines| lines.iter().find(|l| pattern.is_match(&l.text)).map(|l| l.text.clone()));
                match found {
                    Some(line) => {
                        f.insert("line".into(), json!(line));
                    }
                    None => return Err(Failure::new(format!("no log line so far matches /{}/", pattern.as_str()))),
                }
            }
            Kind::ExpectNoLog { pattern } => {
                self.session.log_barrier(LOG_BARRIER).await;
                let found = self.events.with_lines(0, |lines| lines.iter().find(|l| pattern.is_match(&l.text)).map(|l| l.text.clone()));
                if let Some(line) = found {
                    let mut failure = Failure::new(format!("a log line matches /{}/: {line}", pattern.as_str()));
                    failure.fields.insert("line".into(), json!(line));
                    return Err(failure);
                }
            }
            Kind::ExpectShot { name, reference, masks } => {
                let name = expand(name, iters);
                let shot = self.shots.get(&name).cloned().ok_or_else(|| Failure::new(format!("no shot:{name} before this step")))?;
                let a = image::read_png(&shot).map_err(Failure::new)?;
                let b = image::read_png(reference).map_err(Failure::new)?;
                let diff_file = format!("shots/{name}.diff.png");
                let d = image::diff(&a, &b, masks, Some(&self.opts.dir.join(&diff_file))).map_err(Failure::new)?;
                f.insert("name".into(), json!(name));
                f.insert("reference".into(), json!(reference));
                f.insert("diff".into(), serde_json::to_value(&d).unwrap_or_default());
                if !d.size_matches {
                    return Err(Failure { message: format!("shot {name} is {}x{}, the reference {}x{}", a.width, a.height, b.width, b.height), fields: f });
                }
                if !d.equal {
                    f.insert("diff_file".into(), json!(diff_file));
                    let message = format!("shot {name} differs from {}: {} pixels in {} rows (see {diff_file})", reference.display(), d.differing_pixels, d.differing_rows);
                    return Err(Failure { message, fields: f });
                }
            }
            Kind::Hmp { command } => {
                // A reset through the monitor starts a new boot, as `reset` does.
                let resets = command.trim_start().starts_with("system_reset");
                if resets {
                    self.begin_reset().await;
                }
                let out = self.session.qmp("human-monitor-command", Some(json!({"command-line": command}))).await?;
                if resets {
                    self.action().await;
                }
                f.insert("output".into(), json!(out.as_str().unwrap_or_default().trim_end()));
            }
            Kind::Repeat { .. } => unreachable!("repeat is expanded by run_list"),
        }
        Ok(f)
    }

    /// The screen once it has settled after its latest update.
    async fn settled_screen(&mut self) -> Result<crate::session::Framebuffer, Failure> {
        let started = Instant::now();
        loop {
            let updates = self.events.screen().updates;
            if updates > 0 && self.frames.settled() == updates {
                if let Some(fb) = self.session.screen() {
                    return Ok(fb);
                }
            }
            self.check_alive()?;
            if started.elapsed().as_secs_f64() > self.timeout {
                return Err(Failure::new(if updates == 0 {
                    "the firmware has not drawn anything yet; put ready or wait-screen before the shot".to_string()
                } else {
                    format!("the screen kept changing (or the panel stayed busy) for {} s", self.timeout)
                }));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn touch_panel(&self) -> Result<(u32, u32), Failure> {
        let machine = self.machine();
        if machine != "x4pro" && machine != "picco" {
            return Err(Failure::new(format!("the {machine} has no touchscreen; use press")));
        }
        let fb = self.session.screen().ok_or_else(|| Failure::new("no screen yet; put ready before touching"))?;
        Ok((fb.width, fb.height))
    }

    /// Move the pointer (panel pixels) and press or release it.
    async fn pointer(&mut self, x: f64, y: f64, w: u32, h: u32, down: Option<bool>) -> Result<(), Failure> {
        let axis = |v: f64, size: u32| ((v / f64::from(size.saturating_sub(1).max(1))).clamp(0.0, 1.0) * 32767.0).round() as i64;
        let mut events = vec![
            json!({"type": "abs", "data": {"axis": "x", "value": axis(x, w)}}),
            json!({"type": "abs", "data": {"axis": "y", "value": axis(y, h)}}),
        ];
        if let Some(down) = down {
            events.push(json!({"type": "btn", "data": {"down": down, "button": "left"}}));
        }
        self.session.qmp("input-send-event", Some(json!({"events": events}))).await?;
        Ok(())
    }

    fn trace_log(&mut self) -> Result<(), Failure> {
        match self.session.trace_log() {
            Some(Ok(log)) => {
                let run = self.session.info().run;
                self.heap.use_trace(run, log);
                Ok(())
            }
            Some(Err(reason)) => Err(Failure::new(format!("allocation tracing is off: {reason}"))),
            None => Err(Failure::new("the emulator is not running")),
        }
    }

    /// A fresh heap snapshot, with the allocation sites' names; retried
    /// while another reader holds the guest.
    async fn read_heap(&mut self) -> Result<Value, Failure> {
        let started = Instant::now();
        loop {
            self.heap.invalidate();
            let elf = self.opts.elf.clone();
            match self.heap.snapshot(&self.session, elf.as_deref()).await {
                Ok(mut snap) => {
                    snap["siteNames"] = self.heap.sites(0);
                    return Ok(snap);
                }
                Err(e) if e.code == "EBUSY" && started.elapsed() < Duration::from_secs(5) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(Failure::new(format!("reading the heaps failed: {}", e.message))),
            }
        }
    }

    /// Keep what explains a failure in fail/: the record, the screen, the
    /// heaps, the last 200 log lines and the CPU registers.
    async fn failure_bundle(&mut self, record: &Value) -> String {
        let dir = self.opts.dir.join("fail");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("step.json"), serde_json::to_string_pretty(record).unwrap_or_default());
        if let Some(fb) = self.session.screen() {
            let _ = image::write_png(&dir.join("last.png"), fb.width, fb.height, &fb.pixels);
        }
        let _ = std::fs::write(dir.join("log-tail.txt"), self.tail(200).join("\n") + "\n");
        if self.session.machine().is_some() {
            if let Ok(snap) = self.read_heap().await {
                let _ = std::fs::write(dir.join("heap.json"), snap.to_string());
            }
            if let Ok(out) = self.session.qmp("human-monitor-command", Some(json!({"command-line": "info registers -a"}))).await {
                let _ = std::fs::write(dir.join("registers.txt"), out.as_str().unwrap_or_default());
            }
        }
        "fail".into()
    }

    /// Stop recording frames and flush the logs.
    pub fn finish(&mut self) {
        self.frames.stop();
        self.events.flush();
        let _ = self.records.flush();
    }
}

/// Check steps against the board before running them: keys it has, a
/// touchscreen for touch and swipe, --trace for trace steps, and each
/// trace-stop after its trace-start.
pub fn validate(steps: &[Step], machine: &str, traced: bool) -> Result<(), String> {
    fn walk(steps: &[Step], machine: &str, traced: bool, started: &mut Vec<String>) -> Result<(), String> {
        for step in steps {
            match &step.kind {
                Kind::Press { key, .. } => {
                    key_path(machine, key).map_err(|e| format!("`{}`: {e}", step.text))?;
                }
                Kind::Touch { .. } | Kind::Swipe { .. } if machine != "x4pro" && machine != "picco" => {
                    return Err(format!("`{}`: the {machine} has no touchscreen (the X4 Pro and the Picco do); use press", step.text));
                }
                Kind::TraceStart { .. } | Kind::TraceStop { .. } if !traced => {
                    return Err(format!("`{}`: allocation traces need --trace (and the firmware's ELF)", step.text));
                }
                Kind::TraceStart { name } => started.push(name.clone()),
                Kind::TraceStop { name } if !started.contains(name) => {
                    return Err(format!("`{}` has no trace-start:{name} before it", step.text));
                }
                Kind::Repeat { body, .. } => walk(body, machine, traced, started)?,
                _ => {}
            }
        }
        Ok(())
    }
    walk(steps, machine, traced, &mut Vec::new())
}

/// The QOM path and property of a key on a board.
pub fn key_path(machine: &str, key: &str) -> Result<(String, String), String> {
    let (object, keys): (&str, &[&str]) = match machine {
        "x3" | "x4" => ("x3-keys", &["back", "confirm", "left", "right", "up", "down", "power"]),
        "x4pro" => ("x4pro-keys", &["up", "down", "power", "home"]),
        "picco" => ("picco-keys", &["up", "down", "power"]),
        other => return Err(format!("unknown board {other:?}")),
    };
    if !keys.contains(&key) {
        return Err(format!("the {machine} has no {key:?} key; its keys are {}", keys.join(", ")));
    }
    if key == "home" {
        return Ok(("/machine/gt911".into(), "home".into()));
    }
    Ok((format!("/machine/{object}"), key.to_string()))
}

/// The numbers of a heap snapshot a record carries. `free` and `min_free`
/// add up every heap's own counters (multi_heap_info's free bytes and
/// lowest free since boot, what heap_caps_get_free_size and CrossPoint's
/// [MEM] report count); `largest` is the largest free block of any heap
/// (the firmware's MaxAlloc is that less allocator overhead). `dram_*`
/// count the heaps in internal DRAM only; `internal_*` add the RTC heap.
/// With allocation tracing, `live_sites` are the sites holding the most
/// requested bytes in live blocks.
pub fn heap_summary(snap: &Value) -> Fields {
    let mut f = Fields::new();
    let heaps = snap["heaps"].as_array().cloned().unwrap_or_default();
    let n = |h: &Value, k: &str| h[k].as_u64().unwrap_or(0);
    let mut list = Vec::new();
    let (mut free, mut largest, mut min_free) = (0u64, 0u64, 0u64);
    let (mut ifree, mut ilargest, mut dfree, mut dlargest) = (0u64, 0u64, 0u64, 0u64);
    let mut by_site: HashMap<i64, (u64, u64)> = HashMap::new();
    let mut attributed = false;
    for h in &heaps {
        let internal = h["caps"].as_array().is_some_and(|c| c.iter().any(|v| v == "INTERNAL"));
        free += n(h, "infoFree");
        min_free += n(h, "infoMinFree");
        largest = largest.max(n(h, "largest"));
        if internal {
            ifree += n(h, "infoFree");
            ilargest = ilargest.max(n(h, "largest"));
        }
        if h["name"] == "DRAM" {
            dfree += n(h, "infoFree");
            dlargest = dlargest.max(n(h, "largest"));
        }
        if let (Some(blocks), Some(sites)) = (h["blocks"].as_array(), h["sites"].as_array()) {
            attributed = true;
            for (i, b) in blocks.chunks(4).enumerate() {
                let flags = b.get(2).and_then(Value::as_u64).unwrap_or(0) as u32;
                if flags & BLOCK_FREE != 0 {
                    continue;
                }
                let requested = b.get(3).and_then(Value::as_u64).unwrap_or(0);
                let site = sites.get(i).and_then(Value::as_i64).unwrap_or(-1);
                let entry = by_site.entry(site).or_default();
                entry.0 += 1;
                entry.1 += requested;
            }
        }
        list.push(json!({
            "name": h["name"], "start": format!("{:#x}", n(h, "start")), "free": n(h, "infoFree"), "largest": n(h, "largest"),
            "min_free": n(h, "infoMinFree"), "used": n(h, "used"), "blocks": n(h, "count"), "corrupt": n(h, "corrupt"),
        }));
    }
    f.insert("free".into(), json!(free));
    f.insert("largest".into(), json!(largest));
    f.insert("min_free".into(), json!(min_free));
    f.insert("dram_free".into(), json!(dfree));
    f.insert("dram_largest".into(), json!(dlargest));
    f.insert("internal_free".into(), json!(ifree));
    f.insert("internal_largest".into(), json!(ilargest));
    f.insert("heaps".into(), json!(list));
    if attributed {
        let names = snap["siteNames"].as_array().cloned().unwrap_or_default();
        let mut sites: Vec<(i64, (u64, u64))> = by_site.into_iter().collect();
        sites.sort_by_key(|s| std::cmp::Reverse(s.1 .1));
        let top: Vec<Value> = sites
            .iter()
            .take(10)
            .map(|(site, (blocks, bytes))| {
                let name = usize::try_from(*site).ok().and_then(|i| names.get(i));
                json!({
                    "fn": name.map(|n| n["fn"].clone()).unwrap_or(json!("(allocated before tracing, or not traced)")),
                    "loc": name.map(|n| n["loc"].clone()).unwrap_or(json!("")),
                    "blocks": blocks,
                    "bytes": bytes,
                })
            })
            .collect();
        f.insert("live_sites".into(), json!(top));
    }
    f.insert("elf".into(), snap["elf"].clone());
    if !snap["elfNote"].is_null() {
        f.insert("elf_note".into(), snap["elfNote"].clone());
    }
    f.insert("traced".into(), snap["traced"].clone());
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use steps::Coord;

    #[test]
    fn keys_by_board() {
        assert_eq!(key_path("x3", "confirm").unwrap(), ("/machine/x3-keys".into(), "confirm".into()));
        assert_eq!(key_path("x4pro", "home").unwrap().0, "/machine/gt911");
        assert!(key_path("x4pro", "back").unwrap_err().contains("up, down, power, home"));
    }

    #[test]
    fn coords() {
        assert_eq!(Coord::Frac(0.5).resolve(101), 50.0);
        assert_eq!(Coord::Px(7.0).resolve(10), 7.0);
    }

    #[test]
    fn names_expand() {
        assert_eq!(expand("page{i}", &[2, 7]), "page7");
        assert_eq!(expand("page{i}", &[]), "page1");
    }

    #[test]
    fn heap_summary_counts_dram_and_live_sites() {
        let snap = json!({
            "heaps": [
                {"name": "DRAM", "caps": ["INTERNAL"], "infoFree": 100, "infoMinFree": 50, "largest": 80, "start": 0, "count": 2,
                 "blocks": [0, 32, 2, 20, 32, 64, 1, 0], "sites": [0, -1]},
                {"name": "RTC", "caps": ["INTERNAL"], "infoFree": 10, "infoMinFree": 5, "largest": 8, "start": 0, "count": 0}
            ],
            "siteNames": [{"fn": "makeBook", "loc": "book.cpp:1"}]
        });
        let f = heap_summary(&snap);
        assert_eq!(f["free"], 110);
        assert_eq!(f["dram_free"], 100);
        assert_eq!(f["internal_free"], 110);
        assert_eq!(f["live_sites"][0]["fn"], "makeBook");
        assert_eq!(f["live_sites"][0]["bytes"], 20);
    }
}
