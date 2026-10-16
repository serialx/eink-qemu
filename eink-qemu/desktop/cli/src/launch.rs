//! From command-line options to a running session: the run directory, the
//! flash image, the SD card copy and the files staged on it, the ELF, the
//! QEMU binaries and their arguments.

use crate::RunArgs;
use eink_core::cards;
use eink_core::flash::{self, Flash};
use eink_core::heap::Elf;
use eink_core::script::events::RunEvents;
use eink_core::script::GUEST_CLOCK_ID;
use eink_core::sd;
use eink_core::session::{QemuPaths, Session, StartOptions, TracePlan};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MIB: u64 = 1024 * 1024;

pub struct Prepared {
    pub dir: PathBuf,
    pub firmware: PathBuf,
    pub flash: Flash,
    pub elf: Option<PathBuf>,
    pub elf_note: Option<String>,
    pub trace: TracePlan,
    pub card: PathBuf,
    pub card_info: Value,
    pub qemu: QemuPaths,
    pub guest_clock: bool,
    pub options: StartOptions,
    /// Held while the run uses a card in place.
    pub card_lock: Option<cards::Card>,
}

/// A run directory: --out, else eink-qemu-out/<time>-<firmware> beside the
/// working directory, with eink-qemu-out/latest pointing at it.
pub fn run_dir(out: Option<&Path>, firmware: &Path, suffix: &str) -> Result<PathBuf, String> {
    run_dir_in(out, firmware, suffix, false)
}

/// As `run_dir`; with `reuse`, an existing --out is taken as it is (the
/// background run that `start` prepared the directory for).
pub fn run_dir_in(out: Option<&Path>, firmware: &Path, suffix: &str, reuse: bool) -> Result<PathBuf, String> {
    let dir = match out {
        Some(out) => {
            if !reuse && out.exists() && std::fs::read_dir(out).map(|mut d| d.next().is_some()).unwrap_or(false) {
                return Err(format!("{} already exists and is not empty; choose another --out", out.display()));
            }
            std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
            out.to_path_buf()
        }
        None => {
            let stem = firmware
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| s != "firmware")
                .or_else(|| firmware.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "run".into());
            let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
            let time = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let base = PathBuf::from("eink-qemu-out");
            std::fs::create_dir_all(&base).map_err(|e| format!("cannot create {}: {e}", base.display()))?;
            // Each candidate is created exclusively, so two runs never share one.
            let mut n = 1;
            loop {
                let name = if n == 1 { format!("{time}-{stem}{suffix}") } else { format!("{time}-{stem}{suffix}-{n}") };
                let dir = base.join(name);
                match std::fs::create_dir(&dir) {
                    Ok(()) => break dir,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                    Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
                }
            }
        }
    };
    let dir = dunce::canonicalize(&dir).unwrap_or(dir);
    if !reuse {
        // Claims the directory: a second run given the same --out stops here.
        let marker = dir.join(".eink-qemu-run");
        std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("another run is using {}; choose another --out", dir.display())
            } else {
                format!("cannot write in {}: {e}", dir.display())
            }
        })?;
    }
    if out.is_none() {
        point_latest(&dir);
    }
    Ok(dir)
}

/// eink-qemu-out/latest -> `dir` (a symbolic link where possible, else a
/// file naming it).
pub fn point_latest(dir: &Path) {
    let Some(parent) = dir.parent() else { return };
    let latest = parent.join("latest");
    // Made beside it and renamed over it, so `latest` is never missing.
    let temp = parent.join(format!(".latest-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(dir.file_name().unwrap_or_default(), &temp).is_ok() && std::fs::rename(&temp, &latest).is_ok() {
            return;
        }
    }
    if std::fs::write(&temp, dir.to_string_lossy().as_bytes()).is_ok() && std::fs::rename(&temp, &latest).is_err() {
        let _ = std::fs::remove_file(&latest);
        let _ = std::fs::rename(&temp, &latest);
    }
}

/// Where the QEMU binaries are: --qemu, EINK_QEMU_BIN, a build/ or bin/
/// directory above this executable, or the repository's build directory.
pub fn find_qemu(explicit: Option<&Path>, machine: &str) -> Result<QemuPaths, String> {
    let name = if machine == "x3" || machine == "x4" { "qemu-system-riscv32" } else { "qemu-system-xtensa" };
    let exe_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut tried: Vec<PathBuf> = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = explicit {
        candidates.push(dir.to_path_buf());
    } else if let Some(dir) = std::env::var_os("EINK_QEMU_BIN") {
        candidates.push(PathBuf::from(dir));
    } else {
        if let Ok(exe) = std::env::current_exe() {
            for ancestor in exe.ancestors().skip(1).take(7) {
                candidates.push(ancestor.join("build"));
                candidates.push(ancestor.join("bin"));
                candidates.push(ancestor.join("qemu").join("bin"));
            }
        }
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build"));
    }
    for dir in candidates {
        let binary = dir.join(&exe_name);
        if binary.is_file() {
            let dir = dunce::canonicalize(&dir).unwrap_or(dir);
            let plugins = std::env::var_os("EINK_QEMU_PLUGINS").map(PathBuf::from).unwrap_or_else(|| {
                [dir.join("contrib").join("plugins"), dir.join("..").join("plugins"), dir.join("..").join("qemu").join("plugins")]
                    .into_iter()
                    .find(|p| p.is_dir())
                    .unwrap_or_else(|| dir.join("contrib").join("plugins"))
            });
            let share = std::env::var_os("EINK_QEMU_SHARE").map(PathBuf::from);
            return Ok(QemuPaths { bin: dir, share, plugins });
        }
        tried.push(binary);
    }
    Err(format!(
        "{name} was not found (looked for {}). Build QEMU (EINK-QEMU.md: `ninja -C build {name}`), or pass --qemu DIR / set EINK_QEMU_BIN to the directory that holds it.",
        tried.iter().take(4).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

/// Whether this QEMU has the guest-clock object (older builds do not).
fn has_guest_clock(binary: &Path) -> bool {
    std::process::Command::new(binary)
        .args(["-object", "help"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("guest-clock"))
        .unwrap_or(false)
}

/// The ELF for heap sites and traces: --elf PATH, none, or (auto) one
/// beside the firmware that built it.
fn choose_elf(args: &RunArgs, firmware: &Path, flash: &Flash) -> Result<(Option<PathBuf>, Option<String>), String> {
    match args.elf.as_deref() {
        Some("none") => Ok((None, None)),
        Some("auto") | None => {
            let app = flash.app.clone().unwrap_or_else(|| firmware.to_path_buf());
            let mut skipped = None;
            for path in eink_core::firmware::elf_candidates(&app) {
                let Ok(elf) = Elf::load(&path) else { continue };
                match elf.matches(&flash.image) {
                    Some(false) => skipped = Some(format!("{} is from another build of the firmware; left out", path.display())),
                    _ => return Ok((Some(path), None)),
                }
            }
            Ok((None, skipped.or_else(|| Some("no ELF beside the firmware; heap blocks are not attributed to code".into()))))
        }
        Some(path) => {
            let path = PathBuf::from(path);
            let elf = Elf::load(&path).map_err(|e| format!("--elf {}: {e}", path.display()))?;
            if elf.matches(&flash.image) == Some(false) {
                return Err(format!("--elf {} was not built with this firmware (its app descriptor differs)", path.display()));
            }
            Ok((Some(path), None))
        }
    }
}

/// The card lock directory shared with the desktop app (EINK_QEMU_LOCKS overrides it).
pub fn shared_locks() -> PathBuf {
    std::env::var_os("EINK_QEMU_LOCKS").map(PathBuf::from).unwrap_or_else(cards::default_locks_dir)
}

/// A lock error, with what to do when the lock directory is not writable.
pub fn lock_error(what: &str, message: &str, locks: &Path) -> String {
    let denied = message.contains("Operation not permitted") || message.contains("Permission denied") || message.contains("os error 1)") || message.contains("os error 13)");
    if denied {
        format!(
            "{what}: {message} The tool takes card locks in {}, shared with the desktop app so the two never write one card; \
             this process may not write there (a sandbox?). Allow writes to it, or set EINK_QEMU_LOCKS to a writable directory.",
            locks.display()
        )
    } else {
        format!("{what}: {message}")
    }
}

/// Bad options that clap cannot see alone, found before any work starts.
pub fn check_args(args: &RunArgs) -> Result<(), String> {
    if args.sd_in_place && args.sd.is_none() {
        return Err("--sd-in-place needs --sd IMG".into());
    }
    for pattern in &args.crash_pattern {
        regex::Regex::new(pattern).map_err(|e| format!("--crash-pattern {pattern}: {e}"))?;
    }
    if args.trace && args.elf.as_deref() == Some("none") {
        return Err("--trace needs the firmware's ELF; drop --elf none".into());
    }
    Ok(())
}

fn card_for(args: &RunArgs, dir: &Path) -> Result<(PathBuf, Value, Option<cards::Card>), String> {
    let locks = shared_locks();
    if args.sd_in_place {
        let src = args.sd.as_deref().ok_or("--sd-in-place needs --sd IMG")?;
        let card = cards::open_card(&locks, src).map_err(|e| lock_error(&format!("--sd {}", src.display()), &e.message, &locks))?;
        let path = card.path.clone();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        return Ok((path.clone(), json!({"path": path, "source": path, "in_place": true, "size_mib": size / MIB}), Some(card)));
    }
    let card = dir.join("card.img");
    let info = match &args.sd {
        Some(src) => {
            let src = cards::validate_card(src).map_err(|e| format!("--sd {}: {}", src.display(), e.message))?;
            // Copy under the card's lock, so a desktop window is not writing it
            // meanwhile; without a writable lock directory, copy anyway and say so.
            let (lock, lock_note) = match cards::reserve(&locks, &src) {
                Ok(lock) => (Some(lock), None),
                Err(e) if e.code == "EBUSY" => {
                    return Err(format!("--sd {}: {} Close it there or copy it first.", src.display(), e.message));
                }
                Err(e) => (None, Some(lock_error("the card was copied without its lock", &e.message, &locks))),
            };
            let out = std::fs::File::create(&card).map_err(|e| format!("cannot create {}: {e}", card.display()))?;
            sd::sparse_copy(&src, &out).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
            drop(lock);
            let size = std::fs::metadata(&card).map(|m| m.len()).unwrap_or(0);
            json!({"path": card, "source": src, "in_place": false, "size_mib": size / MIB, "lock_note": lock_note})
        }
        None => {
            let size = args.sd_size * MIB;
            sd::check_size(size).map_err(|e| format!("--sd-size {}: {}", args.sd_size, e.message))?;
            let made = sd::create_card(&card, size).map_err(|e| format!("cannot create the SD card: {}", e.message))?;
            json!({"path": card, "source": null, "in_place": false, "size_mib": made.size / MIB, "cluster": made.cluster, "sparse": made.sparse})
        }
    };
    Ok((card, info, None))
}

/// Copy --put files onto the card: HOST[=CARD_DIR], into / by default.
fn stage(card: &Path, puts: &[String]) -> Result<Vec<Value>, String> {
    let mut staged = Vec::new();
    for put in puts {
        let (host, dest) = match put.rsplit_once('=') {
            Some((h, d)) if !d.is_empty() && !h.is_empty() => (h, d.to_string()),
            _ => (put.as_str(), "/".to_string()),
        };
        let dest = if dest.starts_with('/') { dest } else { format!("/{dest}") };
        let src = PathBuf::from(host);
        if !src.exists() {
            return Err(format!("--put {put}: {} does not exist", src.display()));
        }
        // Make the destination folders first.
        let mut path = String::new();
        for part in dest.split('/').filter(|p| !p.is_empty()) {
            path.push('/');
            path.push_str(part);
            match sd::mkdir(card, &path) {
                Ok(()) => {}
                Err(e) if e.code == "EEXIST" => {}
                Err(e) => return Err(format!("--put {put}: cannot make {path} on the card: {}", e.message)),
            }
        }
        let sources = vec![src.clone()];
        let plan = sd::plan_upload(card, &dest, &sources).map_err(|e| format!("--put {put}: {}", e.message))?;
        let done = sd::upload(card, &dest, &sources, &plan.conflicts).map_err(|e| format!("--put {put}: {}", e.message))?;
        staged.push(json!({"host": src, "card_dir": dest, "files": done.files, "bytes": done.bytes}));
    }
    Ok(staged)
}

pub fn prepare(args: &RunArgs, firmware: &Path, dir: PathBuf) -> Result<Prepared, String> {
    if !firmware.exists() {
        return Err(format!("{} does not exist", firmware.display()));
    }
    let flash = flash::make_flash(firmware, args.machine.as_deref()).map_err(|e| format!("{}: {e}", firmware.display()))?;
    let (elf, elf_note) = choose_elf(args, firmware, &flash)?;
    let qemu = find_qemu(args.qemu.as_deref(), &flash.machine)?;
    let trace: TracePlan = if args.trace {
        let elf_path = elf.clone().ok_or_else(|| {
            format!("--trace needs the firmware's ELF: {}", elf_note.clone().unwrap_or_else(|| "pass --elf".into()))
        })?;
        let plugin = qemu.heaptrace().ok_or_else(|| {
            format!("--trace needs the heaptrace plugin, which is not in {} (build it: ninja -C build contrib/plugins/libheaptrace.{})",
                qemu.plugins.display(), if cfg!(target_os = "macos") { "dylib" } else if cfg!(windows) { "dll" } else { "so" })
        })?;
        let parsed = Elf::load(&elf_path).map_err(|e| format!("--trace: {e}"))?;
        if let Some(problem) = parsed.trace_problem(&flash.image) {
            return Err(format!("--trace: {problem}"));
        }
        Ok((plugin, elf_path))
    } else {
        Err("run with --trace to record allocations".into())
    };
    let (card, mut card_info, card_lock) = card_for(args, &dir)?;
    let staged = stage(&card, &args.put)?;
    if !staged.is_empty() {
        card_info["staged"] = json!(staged);
    }
    let guest_clock = has_guest_clock(&qemu.binary(&flash.machine));
    let mut extra: Vec<String> = Vec::new();
    if guest_clock {
        extra.extend(["-object".into(), format!("guest-clock,id={GUEST_CLOCK_ID}")]);
    }
    if args.rtc != "now" {
        extra.extend(["-rtc".into(), format!("base={},clock=vm", args.rtc)]);
    }
    if let Some(port) = args.gdb {
        extra.extend(["-gdb".into(), format!("tcp:127.0.0.1:{port}")]);
    }
    extra.extend(args.qemu_arg.iter().cloned());
    let options = StartOptions {
        machine: flash.machine.clone(),
        panel: args.panel.clone(),
        wifi: args.wifi,
        turbo: args.turbo,
        ghosting: args.ghosting,
        animate: Some(args.animate),
        extra,
    };
    Ok(Prepared { dir, firmware: firmware.to_path_buf(), flash, elf, elf_note, trace, card, card_info, qemu, guest_clock, options, card_lock })
}

/// Start QEMU for a prepared run; the session reports to `events`.
pub async fn launch(p: &mut Prepared, events: Arc<RunEvents>) -> Result<Arc<Session>, String> {
    // The run's own copy is known to this run alone: its lock lives beside it.
    let locks = p.dir.join(".locks");
    let card = match p.card_lock.take() {
        Some(card) => card,
        None => cards::open_card(&locks, &p.card).map_err(|e| lock_error(&format!("SD card {}", p.card.display()), &e.message, &locks))?,
    };
    let session = Session::new(events.clone(), p.qemu.clone(), Some(card));
    let trace = p.trace.clone();
    if let Err(e) = session.start(p.options.clone(), p.flash.image.clone(), trace).await {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let tail = events.with_lines(events.line_count().saturating_sub(12), |l| l.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        return Err(format!("QEMU did not start: {}\n{}", e.message, tail.join("\n")));
    }
    Ok(session)
}

/// The start record: what this run is.
pub fn start_record(p: &Prepared, session: &Session, side: Option<&str>) -> Value {
    let mut r = json!({
        "type": "start",
        "run_dir": p.dir,
        "firmware": p.firmware,
        "machine": p.flash.machine,
        "flash": {"kind": p.flash.kind, "sha256": flash::sha256_hex(&p.flash.image), "app": p.flash.app},
        "elf": p.elf,
        "elf_note": p.elf_note,
        "trace": p.trace.as_ref().ok().map(|(plugin, _)| plugin.clone()),
        "card": p.card_info,
        "timing": if p.options.turbo { "turbo" } else { "accurate" },
        "ghosting": p.options.ghosting,
        "animate": p.options.animate,
        "guest_clock": p.guest_clock,
        "qemu": session.command(),
    });
    if let Some(side) = side {
        r["side"] = json!(side);
    }
    r
}
