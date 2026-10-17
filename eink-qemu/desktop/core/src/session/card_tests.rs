use super::*;
use crate::{cards, sd};
use std::sync::atomic::AtomicUsize;
use tokio::io::AsyncWriteExt;

#[derive(Default)]
struct Sink { resets: AtomicUsize, states: StdMutex<Vec<&'static str>> }
impl Events for Sink {
    fn sd_reset(&self) { self.resets.fetch_add(1, Ordering::SeqCst); }
    fn session(&self, info: &SessionInfo) { self.states.lock().unwrap().push(info.state); }
}

struct Fixture {
    session: Arc<Session>,
    sink: Arc<Sink>,
    old: PathBuf,
    new: PathBuf,
    locks: PathBuf,
    exit: watch::Sender<Option<String>>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    // A monitor with real QMP framing; fail either by rejecting a command or
    // dropping its connection. Process exit is separate from monitor loss.
    async fn running(fail: Option<&'static str>, disconnect: bool, exits: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(dir.path()).unwrap();
        let old = base.join("old.img");
        let new = base.join("new.img");
        let locks = dir.path().join("locks");
        for path in [&old, &new] {
            sd::create_blank(path).unwrap();
            sd::mkdir(path, "/kept").unwrap();
        }
        let sink = Arc::new(Sink::default());
        let session = Session::new(sink.clone(), QemuPaths { bin: ".".into(), plugins: ".".into(), share: None },
            Some(cards::open_card(&locks, &old).unwrap()));
        let (exit, exited) = watch::channel(None);
        let (kill, killed) = oneshot::channel();
        let on_kill = exit.clone();
        tokio::spawn(async move {
            let _ = killed.await;
            if exits { let _ = on_kill.send(Some("killed".into())); }
        });
        let (client, server) = tokio::io::duplex(4096);
        let on_quit = exit.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            writer.write_all(b"{\"QMP\":{}}\n").await.unwrap();
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let command: Value = serde_json::from_str(&line).unwrap();
                let name = command["execute"].as_str().unwrap();
                if fail == Some(name) && disconnect { break; }
                let reply = if fail == Some(name) {
                    json!({"id": command["id"], "error": {"desc": "injected failure"}})
                } else { json!({"id": command["id"], "return": {}}) };
                if writer.write_all(format!("{reply}\n").as_bytes()).await.is_err() { break; }
                if name == "quit" && exits { let _ = on_quit.send(Some("quit".into())); break; }
            }
        });
        let (events, _) = mpsc::unbounded_channel();
        let (closed, _) = oneshot::channel();
        let qmp = Qmp::connect(client, events, closed).await.unwrap();
        let (input, _) = mpsc::unbounded_channel();
        let (paused, _) = watch::channel(false);
        let run = Arc::new(Run {
            id: 1, machine: "x3".into(), qmp: Some(qmp), input, paused, gate: Arc::new(Mutex::new(())),
            kill: StdMutex::new(Some(kill)), exited, trace: Err("test".into()),
            pools: Mutex::new(heap::Pools::new()), command: Vec::new(), dir: tempfile::tempdir().unwrap(),
        });
        session.shared.lock().unwrap().run = Some(run);
        session.set(|info| { info.state = "running"; info.run = 1; });
        Self { session, sink, old, new, locks, exit, _dir: dir }
    }

    async fn failed_start(cancelled: bool, exits: bool) -> (Self, AppError) {
        let f = Self::running(None, false, exits).await;
        let run = f.session.shared.lock().unwrap().run.take().unwrap();
        let mut run = Arc::try_unwrap(run).ok().unwrap();
        run.qmp = None;
        let (cancel, cancelled_rx) = watch::channel(false);
        f.session.shared.lock().unwrap().cancel = Some(cancel);
        f.session.set(|info| { info.state = "starting"; info.machine = Some("x3".into()); });
        let stop = cancelled.then(|| {
            let session = f.session.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                session.stop().await.unwrap();
            })
        });
        let (faults_tx, faults) = mpsc::unbounded_channel();
        let mut exited = f.exit.subscribe();
        tokio::spawn(async move {
            if let Ok(status) = exited.wait_for(|status| status.is_some()).await {
                let _ = faults_tx.send(format!("QEMU exited ({}).", status.as_ref().unwrap()));
            }
        });
        let error = {
            let _ops = f.session.ops.lock().await;
            let result = f.session.connect_run(run, std::future::pending(), cancelled_rx, faults).await;
            f.session.complete_start(result).unwrap_err()
        };
        if let Some(stop) = stop { stop.await.unwrap(); }
        (f, error)
    }

    fn target(&self) -> Card { cards::open_card(&self.locks, &self.new).unwrap() }
    fn locked(&self, path: &Path) -> bool { cards::open_card(&self.locks, path).is_err() }
}

#[tokio::test(start_paused = true)]
async fn switch_failures_keep_old_selection_and_erase_target() {
    for disconnect in [false, true] {
        for step in ["stop", "blockdev-change-medium", "system_reset", "cont"] {
            let f = Fixture::running(Some(step), disconnect, true).await;
            let persisted = StdMutex::new(f.old.clone());
            let result = f.session.select_card(f.target(), |path| {
                *persisted.lock().unwrap() = path.to_owned(); Ok(())
            }).await;
            assert!(result.is_err(), "{step}");
            assert_eq!(f.session.info().state, "idle");
            assert_eq!(f.session.card_path().as_ref(), Some(&f.old));
            assert_eq!(*persisted.lock().unwrap(), f.old);
            assert!(f.locked(&f.old));
            assert!(!f.locked(&f.new));
            assert_eq!(f.sink.resets.load(Ordering::SeqCst), 0);
            f.session.with_card(true, sd::erase).await.unwrap();
            assert!(sd::list(&f.old, "/").unwrap().entries.is_empty());
            assert_eq!(sd::list(&f.new, "/").unwrap().entries[0].name, "kept");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn switch_commits_once_after_resume_or_keeps_old_on_save_failure() {
    for save_fails in [false, true] {
        let f = Fixture::running(None, false, true).await;
        let result = f.session.select_card(f.target(), |_| {
            assert!(f.locked(&f.old) && f.locked(&f.new));
            if save_fails { Err(AppError::new("EIO", "settings write failed")) } else { Ok(()) }
        }).await;
        assert_eq!(result.is_err(), save_fails);
        assert_eq!(f.session.card_path().as_ref(), Some(if save_fails { &f.old } else { &f.new }));
        assert_eq!(f.locked(&f.old), save_fails);
        assert_eq!(f.locked(&f.new), !save_fails);
        assert_eq!(f.sink.resets.load(Ordering::SeqCst), usize::from(!save_fails));
        f.session.stop().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_exit_retains_both_locks_and_refuses_all_card_operations() {
    let f = Fixture::running(Some("system_reset"), true, false).await;
    assert!(f.session.select_card(f.target(), |_| Ok(())).await.is_err());
    assert_eq!(f.session.info().state, "stopping");
    assert!(f.locked(&f.old) && f.locked(&f.new));
    assert!(f.session.with_card(true, sd::erase).await.is_err());
    assert!(f.session.with_card(true, |p| sd::upload(p, "/", &[], &[])).await.is_err());
    assert!(f.session.with_card(true, |p| sd::remove(p, "/kept")).await.is_err());
    assert!(f.session.while_idle(|_| -> Result<(), AppError> { panic!("resize must not run") }).await.is_err());
    f.exit.send(Some("exited later".into())).unwrap();
    f.session.fault(1, "exited later".into()).await;
    assert_eq!(f.session.info().state, "idle");
    assert!(f.locked(&f.old) && !f.locked(&f.new));
}

#[tokio::test(start_paused = true)]
async fn startup_timeout_with_unconfirmed_kill_waits_for_exit() {
    startup_failure_retains_card_until_exit(false).await;
}

#[tokio::test(start_paused = true)]
async fn startup_cancel_with_unconfirmed_kill_waits_for_exit() {
    startup_failure_retains_card_until_exit(true).await;
}

async fn startup_failure_retains_card_until_exit(cancelled: bool) {
    let (f, error) = Fixture::failed_start(cancelled, false).await;
    assert_eq!(error.code, if cancelled { "ECANCELED" } else { "EQEMU" });
    assert!(error.message.contains(if cancelled { "cancelled" } else { "did not connect within" }));
    assert_eq!(f.session.info().state, "stopping");
    assert!(!f.sink.states.lock().unwrap().contains(&"idle"));
    assert!(f.session.shared.lock().unwrap().cancel.is_none());
    let run_dir = f.session.shared.lock().unwrap().run.as_ref().unwrap().dir.path().to_owned();
    assert!(run_dir.is_dir());
    assert!(f.locked(&f.old));
    let busy = AppError::busy();
    for error in [
        f.session.with_card(true, sd::erase).await.unwrap_err(),
        f.session.with_card(true, |p| sd::upload(p, "/", &[], &[])).await.unwrap_err(),
        f.session.with_card(true, |p| sd::remove(p, "/kept")).await.unwrap_err(),
        f.session.select_card(f.target(), |_| panic!("switch must not commit")).await.unwrap_err(),
    ] {
        assert_eq!(error.code, busy.code);
        assert_eq!(error.message, busy.message);
    }
    let resize = f.session.while_idle(|_| -> Result<(), AppError> { panic!("resize must not run") }).await.unwrap_err();
    assert_eq!(resize.code, "EBUSY");
    assert_eq!(resize.message, "Stop the emulator before changing the SD card.");
    assert_eq!(sd::list(&f.old, "/").unwrap().entries[0].name, "kept");
    assert!(f.locked(&f.old) && !f.locked(&f.new));
    f.exit.send(Some("exited later".into())).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while f.session.info().state != "idle" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the fault watcher must observe the late exit");
    assert!(f.session.shared.lock().unwrap().run.is_none());
    assert!(!run_dir.exists());
    f.session.with_card(true, sd::erase).await.unwrap();
    f.session.while_idle(|_| Ok(())).await.unwrap();
    f.session.select_card(f.target(), |_| Ok(())).await.unwrap();
    assert!(!f.locked(&f.old) && f.locked(&f.new));
}

#[tokio::test(start_paused = true)]
async fn startup_failure_with_confirmed_kill_returns_to_idle() {
    for cancelled in [false, true] {
        let (f, error) = Fixture::failed_start(cancelled, true).await;
        assert_eq!(error.code, if cancelled { "ECANCELED" } else { "EQEMU" });
        assert_eq!(f.session.info().state, "idle");
        assert_eq!(f.session.info().exit.unwrap().kind, if cancelled { "stopped" } else { "failed" });
        assert!(f.session.shared.lock().unwrap().run.is_none());
        assert!(f.session.shared.lock().unwrap().cancel.is_none());
        f.session.with_card(true, sd::erase).await.unwrap();
        f.session.select_card(f.target(), |_| Ok(())).await.unwrap();
        assert!(!f.locked(&f.old) && f.locked(&f.new));
    }
}

#[tokio::test(start_paused = true)]
async fn erase_failures_never_change_selection_and_stop_before_further_edits() {
    for step in ["stop", "system_reset", "cont"] {
        let f = Fixture::running(Some(step), true, true).await;
        assert!(f.session.with_card(true, sd::erase).await.is_err());
        assert_eq!(f.session.info().state, "idle");
        assert_eq!(f.session.card_path().as_ref(), Some(&f.old));
        assert!(f.locked(&f.old));
        assert_eq!(sd::list(&f.old, "/").unwrap().entries.is_empty(), step != "stop");
        assert_eq!(sd::list(&f.new, "/").unwrap().entries[0].name, "kept");
    }
    let f = Fixture::running(Some("system_reset"), true, false).await;
    assert!(f.session.with_card(true, sd::erase).await.is_err());
    assert_eq!(f.session.info().state, "stopping");
    assert!(f.session.with_card(true, sd::erase).await.is_err());
    f.exit.send(Some("exited".into())).unwrap();
    f.session.stop().await.unwrap();
}

#[tokio::test]
async fn replaced_card_cannot_be_edited_under_an_old_identity_lock() {
    let f = Fixture::running(None, false, true).await;
    f.session.stop().await.unwrap();
    std::fs::rename(&f.new, &f.old).unwrap();
    assert_eq!(f.session.with_card(true, sd::erase).await.unwrap_err().code, "ESTALE");
    assert!(f.session.while_idle(|_| -> Result<(), AppError> { panic!("resize must not run") }).await.is_err());
    assert_eq!(sd::list(&f.old, "/").unwrap().entries[0].name, "kept");
}

#[tokio::test]
async fn idle_reopen_refreshes_identity_without_releasing_the_path() {
    let f = Fixture::running(None, false, true).await;
    f.session.stop().await.unwrap();
    assert!(!f.session.reopen_card(&f.locks, &f.new).await.unwrap());
    let old_alias = f.old.with_file_name("old-alias.img");
    std::fs::hard_link(&f.old, &old_alias).unwrap();
    let replacement = f.target();
    std::fs::rename(&f.new, &f.old).unwrap();
    assert_eq!(f.session.checked_card_path().unwrap_err().code, "ESTALE");
    assert_eq!(f.session.reopen_card(&f.locks, &f.old).await.unwrap_err().code, "EBUSY");
    assert!(f.locked(&f.old) && f.locked(&old_alias));
    assert_eq!(f.session.checked_card_path().unwrap_err().code, "ESTALE");
    drop(replacement);
    assert!(f.session.reopen_card(&f.locks, &f.old).await.unwrap());
    assert_eq!(f.session.checked_card_path().unwrap(), f.old);
    assert!(f.locked(&f.old));
    assert!(!f.locked(&old_alias));
    let new_alias = f.old.with_file_name("new-alias.img");
    std::fs::hard_link(&f.old, &new_alias).unwrap();
    assert!(f.locked(&new_alias));
    f.session.with_card(true, sd::erase).await.unwrap();
    assert!(sd::list(&f.old, "/").unwrap().entries.is_empty());
    assert_eq!(sd::list(&old_alias, "/").unwrap().entries[0].name, "kept");
}

#[tokio::test(start_paused = true)]
async fn reopen_retains_reservations_until_qemu_confirms_exit() {
    for stopping in [false, true] {
        let f = Fixture::running(Some("system_reset"), true, false).await;
        if stopping {
            assert!(f.session.select_card(f.target(), |_| Ok(())).await.is_err());
            assert_eq!(f.session.info().state, "stopping");
        }
        let old_alias = f.old.with_file_name("old-alias.img");
        let replacement = f.old.with_file_name("replacement.img");
        std::fs::hard_link(&f.old, &old_alias).unwrap();
        sd::create_blank(&replacement).unwrap();
        std::fs::rename(&replacement, &f.old).unwrap();
        let error = f.session.reopen_card(&f.locks, &f.old).await.unwrap_err();
        assert_eq!(error.code, "EBUSY");
        assert!(error.message.contains("Stop the emulator"));
        assert!(f.locked(&f.old) && f.locked(&old_alias));
        assert_eq!(f.locked(&f.new), stopping);
        f.exit.send(Some("exited later".into())).unwrap();
        f.session.fault(1, "exited later".into()).await;
        assert_eq!(f.session.info().state, "idle");
        assert!(f.session.reopen_card(&f.locks, &f.old).await.unwrap());
        assert_eq!(f.session.checked_card_path().unwrap(), f.old);
        assert!(f.locked(&f.old));
        assert!(!f.locked(&old_alias) && !f.locked(&f.new));
    }
}

#[tokio::test(start_paused = true)]
async fn partial_erase_stops_but_precheck_failure_resumes_the_same_card() {
    let f = Fixture::running(None, false, true).await;
    let refused: Result<(), AppError> = f.session.with_card(true, |_| Err(SdError::new("EACCES", "read-only card"))).await;
    assert!(refused.is_err());
    assert_eq!(f.session.info().state, "running");
    assert_eq!(f.sink.resets.load(Ordering::SeqCst), 0);
    assert!(f.session.with_card(true, |path| {
        sd::fail_commit_after(Some(512));
        let result = sd::erase(path);
        sd::fail_commit_after(None);
        result
    }).await.is_err());
    assert_eq!(f.session.info().state, "idle");
    assert_eq!(f.session.card_path().as_ref(), Some(&f.old));
    assert!(f.locked(&f.old));
    assert_eq!(sd::list(&f.new, "/").unwrap().entries[0].name, "kept");
}

#[tokio::test]
async fn losing_the_exit_observer_does_not_confirm_process_exit() {
    let (sender, exited) = watch::channel(None);
    drop(sender);
    assert!(!terminate(None, &StdMutex::new(None), &exited).await);
}

#[tokio::test(start_paused = true)]
async fn a_panicking_edit_cannot_resume_the_guest() {
    let f = Fixture::running(None, false, true).await;
    let result: Result<(), AppError> = f.session.with_card(true, |_| panic!("injected edit panic")).await;
    assert!(result.unwrap_err().message.contains("may be damaged"));
    assert_eq!(f.session.info().state, "idle");
    assert!(f.locked(&f.old));
}
