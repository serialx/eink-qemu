//! Long-lived sessions. `run --hold` keeps the emulator running after its
//! steps and takes more over a control socket; `start` launches such a run
//! in the background and returns once its first steps are done; `do` sends
//! steps to it, `stop` ends it, `status` reports on it.
//!
//! The run directory's session.json names the process and its socket.
//! Requests and replies are JSON lines: {"steps": [...]} answered by the
//! steps' records and {"type":"done",...}; {"stop": true}; {"status": true}.

use crate::launch;
use crate::output::Printer;
use crate::run::{collect_steps, Tap};
use crate::{RunArgs, EXIT_FAIL, EXIT_SETUP, EXIT_USAGE};
use eink_core::script::steps;
use eink_core::script::Runner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const SESSION_FILE: &str = "session.json";

enum Listener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener, PathBuf),
    #[allow(dead_code)]
    Tcp(tokio::net::TcpListener),
}

impl Listener {
    async fn bind() -> std::io::Result<(Listener, String)> {
        #[cfg(unix)]
        {
            let path = PathBuf::from(format!("/tmp/eink-qemu-ctl-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let listener = tokio::net::UnixListener::bind(&path)?;
            let address = format!("unix:{}", path.display());
            Ok((Listener::Unix(listener, path), address))
        }
        #[cfg(not(unix))]
        {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = format!("tcp:{}", listener.local_addr()?);
            Ok((Listener::Tcp(listener), address))
        }
    }

    async fn accept(&self) -> std::io::Result<Box<dyn Stream>> {
        match self {
            #[cfg(unix)]
            Listener::Unix(l, _) => Ok(Box::new(l.accept().await?.0)),
            Listener::Tcp(l) => Ok(Box::new(l.accept().await?.0)),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Listener::Unix(_, path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn connect(address: &str) -> std::io::Result<Box<dyn Stream>> {
    if let Some(path) = address.strip_prefix("unix:") {
        #[cfg(unix)]
        return Ok(Box::new(tokio::net::UnixStream::connect(path).await?));
        #[cfg(not(unix))]
        return Err(std::io::Error::other(format!("{path}: Unix sockets are not available here")));
    }
    let addr = address.strip_prefix("tcp:").unwrap_or(address);
    Ok(Box::new(tokio::net::TcpStream::connect(addr).await?))
}

fn write_session(dir: &Path, value: &Value) {
    let _ = std::fs::write(dir.join(SESSION_FILE), serde_json::to_string_pretty(value).unwrap_or_default());
}

/// Keep the session for `do` requests until `stop`, an interrupt, QEMU's
/// exit, or --hold-timeout without a request.
pub async fn serve(runner: &mut Runner, args: &RunArgs, tap: Tap, ok: bool, printer: &Printer) -> bool {
    let dir = runner.dir().to_path_buf();
    let (listener, address) = match Listener::bind().await {
        Ok(l) => l,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "hold", "error": format!("cannot open the control socket: {e}")}));
            return false;
        }
    };
    let mut info = json!({
        "pid": std::process::id(),
        "socket": address,
        "state": if ok { "ready" } else { "ready-after-failure" },
        "run_dir": dir,
        "machine": runner.session.machine(),
    });
    write_session(&dir, &info);
    if let Some(ready) = &args.ready_file {
        let _ = std::fs::write(ready, json!({"ok": ok, "socket": address}).to_string());
    }
    printer.note(&format!(
        "Session held: eink-qemu do --session {} STEP...   eink-qemu stop --session {}",
        dir.display(),
        dir.display()
    ));
    let idle = Duration::from_secs(args.hold_timeout.max(1));
    let ended = loop {
        let accepted = tokio::select! {
            a = tokio::time::timeout(idle, listener.accept()) => a,
            _ = tokio::signal::ctrl_c() => break "interrupted",
            _ = alive_watch(runner) => break "qemu-exited",
        };
        let mut stream = match accepted {
            Err(_) => break "idle-timeout",
            Ok(Err(_)) => continue,
            Ok(Ok(stream)) => stream,
        };
        let request = {
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            match tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await {
                Ok(Ok(n)) if n > 0 => serde_json::from_str::<Value>(&line).unwrap_or(Value::Null),
                _ => continue,
            }
        };
        if request["stop"] == true {
            let _ = reply(&mut stream, &json!({"type": "done", "ok": true, "code": 0, "stopped": true})).await;
            break "stopped";
        }
        if request["status"] == true {
            let status = json!({"type": "status", "running": runner.session.machine().is_some(), "steps": runner.index, "run_dir": dir});
            let _ = reply(&mut stream, &status).await;
            continue;
        }
        let tokens: Vec<String> = request["steps"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let traced = matches!(runner.session.trace_log(), Some(Ok(_)));
        let machine = runner.session.machine().unwrap_or_default();
        let parsed = match steps::parse(&tokens).and_then(|p| {
            // Trace windows started by earlier requests count too.
            let mut all = runner_trace_starts(&runner.trace_names);
            all.extend(p.iter().cloned());
            eink_core::script::validate(&all, &machine, traced).map(|()| p)
        }) {
            Ok(p) => p,
            Err(e) => {
                let _ = reply(&mut stream, &json!({"type": "error", "stage": "usage", "error": e})).await;
                let _ = reply(&mut stream, &json!({"type": "done", "ok": false, "code": EXIT_USAGE})).await;
                continue;
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        *tap.lock().unwrap() = Some(tx);
        let (mut read_half, mut write_half) = tokio::io::split(stream);
        let forward = tokio::spawn(async move {
            while let Some(record) = rx.recv().await {
                if reply(&mut write_half, &record).await.is_err() {
                    break;
                }
            }
            write_half
        });
        remember_trace_starts(&mut runner.trace_names, &parsed);
        let passed = runner.run(&parsed).await;
        tap.lock().unwrap().take();
        if let Ok(mut write_half) = forward.await {
            let code = if passed { 0 } else { EXIT_FAIL };
            let _ = reply(&mut write_half, &json!({"type": "done", "ok": passed, "code": code})).await;
            let _ = write_half.shutdown().await;
        }
        let _ = &mut read_half;
    };
    info["state"] = json!("stopped");
    info["ended"] = json!(ended);
    write_session(&dir, &info);
    runner.emit(&json!({"type": "hold", "ended": ended}));
    true
}

/// trace-start steps for windows opened by earlier requests.
fn runner_trace_starts(names: &[String]) -> Vec<steps::Step> {
    names.iter().filter_map(|n| steps::parse(&[format!("trace-start:{n}")]).ok()).flatten().collect()
}

pub fn remember_trace_starts(names: &mut Vec<String>, parsed: &[steps::Step]) {
    for step in parsed {
        match &step.kind {
            steps::Kind::TraceStart { name } => names.push(name.clone()),
            steps::Kind::Repeat { body, .. } => remember_trace_starts(names, body),
            _ => {}
        }
    }
}

/// Resolves when the emulator stops running.
async fn alive_watch(runner: &Runner) {
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if runner.events.state().is_some_and(|s| s.state != "running" && s.state != "starting") {
            return;
        }
    }
}

async fn reply<W: AsyncWrite + Unpin>(w: &mut W, value: &Value) -> std::io::Result<()> {
    w.write_all(format!("{value}\n").as_bytes()).await?;
    w.flush().await
}

/// The session directory: --session, else eink-qemu-out/latest.
fn session_dir(session: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(dir) = session {
        return Ok(dir);
    }
    let latest = Path::new("eink-qemu-out").join("latest");
    if latest.is_dir() {
        return Ok(latest);
    }
    if let Ok(text) = std::fs::read_to_string(&latest) {
        return Ok(PathBuf::from(text.trim()));
    }
    Err("no session here: start one with `eink-qemu start FIRMWARE ready`, or pass --session DIR".into())
}

fn read_session(dir: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(dir.join(SESSION_FILE))
        .map_err(|_| format!("{} has no session (no {SESSION_FILE}); start one with `eink-qemu start`", dir.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", dir.join(SESSION_FILE).display()))
}

async fn request(dir: &Path, value: &Value) -> Result<Box<dyn Stream>, String> {
    let info = read_session(dir)?;
    if info["state"] == "stopped" {
        return Err(format!("the session in {} has ended ({}); start another with `eink-qemu start`", dir.display(), info["ended"].as_str().unwrap_or("stopped")));
    }
    let address = info["socket"].as_str().unwrap_or_default();
    let mut stream = connect(address)
        .await
        .map_err(|e| format!("the session in {} is not running ({e}); start another with `eink-qemu start`", dir.display()))?;
    reply(&mut stream, value).await.map_err(|e| e.to_string())?;
    Ok(stream)
}

pub async fn send(session: Option<PathBuf>, tokens: Vec<String>) -> u8 {
    let printer = Printer::new(false, false);
    if let Err(e) = steps::parse(&tokens) {
        printer.record(&json!({"type": "error", "stage": "usage", "error": e}));
        return EXIT_USAGE;
    }
    let dir = match session_dir(session) {
        Ok(d) => d,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    };
    let stream = match request(&dir, &json!({"steps": tokens})).await {
        Ok(s) => s,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    };
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(record) = serde_json::from_str::<Value>(&line) else { continue };
        if record["type"] == "done" {
            return record["code"].as_u64().unwrap_or(1) as u8;
        }
        printer.record(&record);
    }
    printer.record(&json!({"type": "error", "stage": "session", "error": "the session closed the connection before finishing (did QEMU exit?)"}));
    EXIT_FAIL
}

pub async fn stop(session: Option<PathBuf>) -> u8 {
    let printer = Printer::new(false, false);
    let dir = match session_dir(session) {
        Ok(d) => d,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    };
    let info = read_session(&dir).unwrap_or(Value::Null);
    match request(&dir, &json!({"stop": true})).await {
        Ok(stream) => {
            let mut lines = BufReader::new(stream).lines();
            let _ = tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await;
        }
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    }
    // Wait for the process to write its summary and exit.
    if let Some(pid) = info["pid"].as_u64() {
        for _ in 0..100 {
            if !eink_core::host::process_alive(pid as u32) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    printer.record(&json!({"type": "stopped", "run_dir": dunce::canonicalize(&dir).unwrap_or(dir.clone()), "report": dir.join("report.html")}));
    0
}

pub async fn status(session: Option<PathBuf>) -> u8 {
    let printer = Printer::new(false, false);
    let dir = match session_dir(session) {
        Ok(d) => d,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    };
    let mut info = match read_session(&dir) {
        Ok(i) => i,
        Err(e) => {
            printer.record(&json!({"type": "error", "stage": "session", "error": e}));
            return EXIT_SETUP;
        }
    };
    let alive = info["state"] != "stopped" && connect(info["socket"].as_str().unwrap_or_default()).await.is_ok();
    info["type"] = json!("status");
    info["alive"] = json!(alive);
    printer.record(&info);
    if alive { 0 } else { EXIT_FAIL }
}

/// Launch `run --hold` in the background and return once its first steps ran.
pub async fn start(firmware: PathBuf, tokens: Vec<String>, args: RunArgs) -> u8 {
    let printer = Printer::new(args.quiet, args.progress);
    if args.gui {
        return crate::run::usage(&printer, "start does not take --gui; use `eink-qemu run --gui` to watch a run");
    }
    if let Err(e) = collect_steps(&args, &tokens) {
        return crate::run::usage(&printer, &e);
    }
    let dir = match launch::run_dir(args.out.as_deref(), &firmware, "") {
        Ok(d) => d,
        Err(e) => return crate::run::usage(&printer, &e),
    };
    let ready = dir.join("ready.json");
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = argv.iter().position(|a| a == "start") {
        argv[i] = "run".into();
    }
    if args.out.is_none() {
        argv.extend(["--out".into(), dir.to_string_lossy().into_owned()]);
    }
    argv.extend(["--hold".into(), "--quiet".into(), "--ready-file".into(), ready.to_string_lossy().into_owned()]);
    let log = match std::fs::File::create(dir.join("session.log")) {
        Ok(f) => f,
        Err(e) => return crate::run::usage(&printer, &format!("cannot write {}: {e}", dir.join("session.log").display())),
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eink-qemu"));
    let mut command = std::process::Command::new(exe);
    command.args(&argv).stdin(std::process::Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own, so closing the terminal does not end it.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return crate::run::usage(&printer, &format!("cannot start the session: {e}")),
    };
    let outcome = loop {
        if let Ok(text) = std::fs::read_to_string(&ready) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                break Some(v);
            }
        }
        if let Ok(Some(_)) = child.try_wait() {
            // It may have written the file just before exiting.
            break std::fs::read_to_string(&ready).ok().and_then(|t| serde_json::from_str(&t).ok());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if let Ok(text) = std::fs::read_to_string(dir.join("run.jsonl")) {
        for line in text.lines() {
            if let Ok(record) = serde_json::from_str::<Value>(line) {
                printer.record(&record);
            }
        }
    }
    match outcome {
        Some(v) if v["socket"].is_string() => {
            let ok = v["ok"] == true;
            printer.record(&json!({
                "type": "session",
                "ok": ok,
                "run_dir": dir,
                "pid": child.id(),
                "next": format!("eink-qemu do --session {} STEP...; eink-qemu stop --session {}", dir.display(), dir.display()),
            }));
            if ok { 0 } else { EXIT_FAIL }
        }
        Some(v) => {
            printer.record(&json!({"type": "error", "stage": "setup", "error": v["error"], "run_dir": dir}));
            EXIT_SETUP
        }
        None => {
            let log = std::fs::read_to_string(dir.join("session.log")).unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
            printer.record(&json!({"type": "error", "stage": "setup", "error": "the session exited before it was ready", "log_tail": tail, "run_dir": dir}));
            EXIT_SETUP
        }
    }
}
