use super::*;
use crate::script::{events::RunEvents, steps, RunOptions, Runner};
use std::io::Write;
use tokio::io::AsyncWriteExt;

struct Monitor {
    running: bool,
    guest_ns: i64,
    commands: Vec<String>,
    fail: Option<&'static str>,
}

struct Fixture {
    session: Arc<Session>,
    events: Arc<RunEvents>,
    monitor: Arc<StdMutex<Monitor>>,
    log: PathBuf,
    dir: tempfile::TempDir,
}

fn alloc(path: &Path, size: u32) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    let ptr = file.metadata().unwrap().len() as u32;
    let mut record = vec![1, 0, 0, 0]; // alloc, no stack
    for value in [ptr, size, 0] { record.extend(value.to_le_bytes()); }
    record.extend(1000u64.to_le_bytes()); // all host timestamps are identical
    file.write_all(&record).unwrap();
}

impl Fixture {
    async fn new(running: bool, fail: Option<&'static str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("heaptrace.bin");
        std::fs::write(&log, b"HTRC\x02\0\0\0").unwrap();
        let events = Arc::new(RunEvents::new(dir.path(), false).unwrap());
        let session = Session::new(events.clone(), QemuPaths { bin: ".".into(), plugins: ".".into(), share: None }, None);
        let monitor = Arc::new(StdMutex::new(Monitor { running, guest_ns: 0, commands: Vec::new(), fail }));
        let state = monitor.clone();
        let output = log.clone();
        let (exit, exited) = watch::channel(None);
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            writer.write_all(b"{\"QMP\":{}}\n").await.unwrap();
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let command: Value = serde_json::from_str(&line).unwrap();
                let name = command["execute"].as_str().unwrap();
                let reply = {
                    let mut state = state.lock().unwrap();
                    state.commands.push(name.to_string());
                    if state.fail == Some(name) {
                        json!({"id": command["id"], "error": {"desc": "injected failure"}})
                    } else {
                        let value = match name {
                            "query-status" => json!({"running": state.running}),
                            "stop" => { state.running = false; json!({}) }
                            "cont" => {
                                state.running = true;
                                // The guest executes before the cont reply reaches the runner.
                                state.guest_ns += 500_000_000;
                                alloc(&output, 16);
                                json!({})
                            }
                            "qom-get" => json!(state.guest_ns),
                            "human-monitor-command" => {
                                assert!(state.running);
                                state.guest_ns += 1_500_000_000;
                                alloc(&output, 32);
                                json!("")
                            }
                            _ => json!({}),
                        };
                        json!({"id": command["id"], "return": value})
                    }
                };
                if writer.write_all(format!("{reply}\n").as_bytes()).await.is_err() { break; }
                if name == "quit" { let _ = exit.send(Some("quit".into())); break; }
            }
        });
        let (qmp_events, _) = mpsc::unbounded_channel();
        let (closed, _) = oneshot::channel();
        let qmp = Qmp::connect(client, qmp_events, closed).await.unwrap();
        let (input, _) = mpsc::unbounded_channel();
        let (paused, _) = watch::channel(false);
        let run = Arc::new(Run {
            id: 1, machine: "x3".into(), qmp: Some(qmp), input, paused, gate: Arc::new(Mutex::new(())),
            kill: StdMutex::new(None), exited, trace: Ok(log.clone()),
            pools: Mutex::new(heap::Pools::new()), command: Vec::new(), dir: tempfile::tempdir().unwrap(),
        });
        session.shared.lock().unwrap().run = Some(run);
        session.set(|info| { info.state = "running"; info.run = 1; });
        Self { session, events, monitor, log, dir }
    }
}

#[tokio::test]
async fn script_windows_capture_guest_time_and_exclude_allocations_after_resume() {
    for clock in [true, false] {
        let f = Fixture::new(true, None).await;
        let records = Arc::new(StdMutex::new(Vec::new()));
        let sink = records.clone();
        let mut runner = Runner::new(f.session.clone(), f.events.clone(), clock, RunOptions::new(f.dir.path().into()),
            Box::new(move |r| sink.lock().unwrap().push(r.clone()))).unwrap();
        let script = steps::parse(&["repeat:2", "trace-start:down{i}", "hmp:work", "trace-stop:down{i}", "end"].map(str::to_string)).unwrap();
        assert!(runner.run(&script).await, "{:?}", runner.failed);
        runner.frames.stop();
        let records = records.lock().unwrap();
        for (i, group) in records.as_chunks::<3>().0.iter().enumerate() {
            let (start, work, stop) = (&group[0], &group[1], &group[2]);
            assert_eq!(start["name"], format!("down{}", i + 1));
            assert_eq!(start["name"], stop["name"]);
            assert_eq!(stop["guest_ms"], work["guest_ms"]);
            if clock {
                assert_eq!(stop["window_guest_ms"], 2000);
                assert_eq!(stop["guest_ms"].as_i64().unwrap() - start["guest_ms"].as_i64().unwrap(), 2000);
            } else {
                assert!(stop["window_guest_ms"].is_null());
                assert!(stop["guest_ms"].is_null());
            }
            assert!(stop.get("window_host_ms").is_some());
            assert!(stop.get("window_ms").is_none());
            assert_eq!(stop["allocs"], 2);
            assert_eq!(stop["kept_bytes"], 48);
            let window: Value = serde_json::from_slice(&std::fs::read(f.dir.path().join(stop["file"].as_str().unwrap())).unwrap()).unwrap();
            assert_eq!(window["kept_bytes"], 48);
        }
        assert_eq!(records.len(), 6);
        assert!(f.monitor.lock().unwrap().running);
        let commands = &f.monitor.lock().unwrap().commands;
        assert_eq!(commands.iter().filter(|c| *c == "stop").count(), 4);
        assert_eq!(commands.iter().filter(|c| *c == "cont").count(), 4);
    }
}

#[tokio::test]
async fn measurement_errors_resume_and_already_stopped_guests_stay_stopped() {
    for running in [true, false] {
        let f = Fixture::new(running, None).await;
        let error = f.session.with_guest_stopped(|| async {
            assert!(!f.monitor.lock().unwrap().running);
            assert!(f.session.ops.try_lock().is_err());
            assert!(f.session.current().unwrap().gate.try_lock().is_err());
            Ok(std::fs::metadata(f.log.with_extension("missing"))?.len())
        }).await.unwrap_err();
        assert_eq!(error.code, "EIO");
        let monitor = f.monitor.lock().unwrap();
        assert_eq!(monitor.running, running);
        assert_eq!(monitor.commands.iter().any(|c| c == "cont"), running);
        assert_eq!(monitor.commands.iter().any(|c| c == "stop"), running);
        assert!(f.session.ops.try_lock().is_ok());
    }
}

#[tokio::test]
async fn measurement_stop_or_resume_failure_ends_the_session() {
    for command in ["stop", "cont"] {
        let f = Fixture::new(true, Some(command)).await;
        let result = f.session.with_guest_stopped(|| async { Ok(()) }).await;
        assert_eq!(result.unwrap_err().code, "EQMP");
        assert_eq!(f.session.info().state, "idle");
    }
}
