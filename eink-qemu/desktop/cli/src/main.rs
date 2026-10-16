//! eink-qemu: run e-ink reader firmware in QEMU from the command line.
//! Scripted, headless runs with screenshots, heap snapshots and allocation
//! traces; paired comparisons of two builds; long-lived sessions driven
//! step by step; and the desktop window on a scripted run.

mod compare;
mod control;
mod gui;
mod launch;
mod output;
mod report;
mod run;
mod sdcmd;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit codes: 0 every step passed, 1 a step failed, 2 bad usage or steps,
/// 3 the run could not start (firmware, card, QEMU), 130 interrupted.
pub const EXIT_FAIL: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_SETUP: u8 = 3;
pub const EXIT_INTERRUPTED: u8 = 130;

/// Seconds for a time limit: more than 0, at most a day.
fn seconds(text: &str) -> Result<f64, String> {
    let v: f64 = text.parse().map_err(|_| format!("{text:?} is not a number of seconds"))?;
    if !v.is_finite() || v <= 0.0 || v > 86_400.0 {
        return Err("use a number of seconds above 0, at most 86400".into());
    }
    Ok(v)
}

/// Milliseconds: 0 to an hour.
fn millis(text: &str) -> Result<u64, String> {
    let v: u64 = text.parse().map_err(|_| format!("{text:?} is not a whole number of milliseconds"))?;
    if v > 3_600_000 {
        return Err("at most 3600000 ms".into());
    }
    Ok(v)
}

/// The real-time clock's start: YYYY-MM-DDTHH:MM:SS, YYYY-MM-DD, or now.
fn rtc(text: &str) -> Result<String, String> {
    if text == "now" {
        return Ok(text.into());
    }
    let ok = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").is_ok()
        || chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
    if ok { Ok(text.into()) } else { Err("use YYYY-MM-DDTHH:MM:SS (or YYYY-MM-DD, or now)".into()) }
}

const AFTER_HELP: &str = "\
Steps run in order; `eink-qemu steps` lists them. A typical run:

  eink-qemu run .pio/build/default --put book.epub ready press:confirm \\
      wait-screen press:confirm 'wait-log:Page render' shot:opened heap:opened

On a blank card Home has Browse Files selected: the first confirm opens the
file list, the second opens its first book. Each step prints one JSON record
on stdout; the run directory (printed in the first record, default
eink-qemu-out/<time>-<name>, linked from eink-qemu-out/latest) keeps
run.jsonl, log.txt, frames/, shots/, heaps/ and report.html. Exit codes:
0 passed, 1 a step failed, 2 bad usage, 3 the run could not start,
130 interrupted. Guide: eink-qemu/desktop/cli/README.md.";

#[derive(Parser)]
#[command(name = "eink-qemu", version, about = "Run e-ink reader firmware (Xteink X3, X4, X4 Pro, BOOX Picco) in QEMU from the command line", after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Boot a firmware and run steps against it, headless (or in the desktop window with --gui).
    #[command(after_help = AFTER_HELP)]
    Run {
        /// Firmware: an app or merged .bin, a PlatformIO/ESP-IDF build directory, or a 16 MiB flash image.
        firmware: PathBuf,
        /// Steps, e.g. ready press:down 'wait-log:Page render' shot:page2 (see `eink-qemu steps`).
        steps: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Run the same steps on two firmwares side by side and compare timings, heaps and screenshots.
    Compare {
        /// The baseline firmware (or build directory).
        base: PathBuf,
        /// The candidate firmware (or build directory).
        candidate: PathBuf,
        /// Steps for both runs.
        steps: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
        /// Names of the two runs in records, folders and tables.
        #[arg(long, default_value = "base,candidate", value_name = "A,B")]
        names: String,
        /// Fail (exit 1) when same-named shots differ.
        #[arg(long)]
        require_identical: bool,
        /// Run one after the other instead of at the same time.
        #[arg(long)]
        sequential: bool,
    },
    /// Boot a firmware in the background and keep it running; drive it with `do`, end it with `stop`.
    Start {
        firmware: PathBuf,
        /// Steps to run before returning, e.g. ready.
        steps: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Run steps on a session started with `start` (or `run --hold`).
    Do {
        /// Steps, e.g. press:down wait-screen shot:next.
        #[arg(required = true)]
        steps: Vec<String>,
        /// The session's run directory (default: eink-qemu-out/latest).
        #[arg(long, value_name = "DIR")]
        session: Option<PathBuf>,
    },
    /// Stop a session started with `start` (or `run --hold`).
    Stop {
        /// The session's run directory (default: eink-qemu-out/latest).
        #[arg(long, value_name = "DIR")]
        session: Option<PathBuf>,
    },
    /// Show whether a session is running and where its files are.
    Status {
        #[arg(long, value_name = "DIR")]
        session: Option<PathBuf>,
    },
    /// Write report.html for a run or comparison directory.
    Report {
        dir: PathBuf,
    },
    /// List the steps a run takes.
    Steps,
    /// Compose the emulator's 16 MiB flash image (like mkflash.py).
    Flash {
        firmware: PathBuf,
        output: PathBuf,
        #[arg(long)]
        machine: Option<String>,
    },
    /// Make, list and edit SD card images.
    Sd {
        #[command(subcommand)]
        command: sdcmd::SdCommand,
    },
}

/// How a run is set up. Options may come before or after the firmware.
#[derive(Args, Clone)]
pub struct RunArgs {
    /// Read more steps from FILE, one per line (# comments; no quoting needed). Run before the command-line steps.
    #[arg(long, value_name = "FILE")]
    pub script: Option<PathBuf>,
    /// Run directory (default: eink-qemu-out/<time>-<firmware>).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// SD card image to start from; the run uses a copy (see --sd-in-place). Default: a blank card.
    #[arg(long, value_name = "IMG")]
    pub sd: Option<PathBuf>,
    /// Use --sd itself, so the firmware's changes stay on it.
    #[arg(long)]
    pub sd_in_place: bool,
    /// Size of a blank card in MiB (2050 is the stock X3 card; timing comparisons need its 32 KiB clusters).
    #[arg(long, default_value_t = 2050, value_name = "MIB", value_parser = clap::value_parser!(u64).range(2050..=32768))]
    pub sd_size: u64,
    /// Copy a host file or folder onto the card before boot: HOST or HOST=/CARD/DIR (default /). Repeatable.
    #[arg(long, value_name = "HOST[=DIR]")]
    pub put: Vec<String>,
    /// Board: x3, x4, x4pro or picco. Default: from the firmware's chip (ESP32-C3 = x3, ESP32-S3 = x4pro).
    #[arg(long, value_parser = ["x3", "x4", "x4pro", "picco"])]
    pub machine: Option<String>,
    /// X4 Pro display panel: uc8279-68 or uc8179.
    #[arg(long, default_value = "uc8279-68", value_parser = ["uc8279-68", "uc8179"])]
    pub panel: String,
    /// The firmware's ELF for heap sites and --trace: auto (beside the firmware), none, or a path.
    #[arg(long, value_name = "auto|none|PATH")]
    pub elf: Option<String>,
    /// Record every allocation with its call stack (heaptrace plugin; needs the ELF). Enables trace-start/trace-stop.
    #[arg(long)]
    pub trace: bool,
    /// Default time limit of waits, in host seconds.
    #[arg(long, default_value_t = 60.0, value_name = "SECONDS", value_parser = seconds)]
    pub timeout: f64,
    /// Time limit of `ready`, in host seconds.
    #[arg(long, default_value_t = 180.0, value_name = "SECONDS", value_parser = seconds)]
    pub ready_timeout: f64,
    /// How long the display must stay unchanged to count as settled (shots, wait-screen, frames), in host ms.
    /// The panel must also have been idle (not BUSY) for --panel-idle.
    #[arg(long, default_value_t = 200, value_name = "MS", value_parser = millis)]
    pub settle: u64,
    /// How long the panel must have been idle after a refresh, in guest ms, before the screen counts as settled.
    #[arg(long, default_value_t = 300, value_name = "MS", value_parser = millis)]
    pub panel_idle: u64,
    /// Before each press, touch or swipe, wait (up to 3 s) until the firmware's log has been quiet and the panel
    /// idle for this many guest ms: firmware busy with background work can miss a short press. 0 turns it off.
    #[arg(long, default_value_t = 200, value_name = "MS", value_parser = millis)]
    pub input_quiet: u64,
    /// Fast peripherals (SD, panel, SPI): quicker runs without real device timing. Not for timing comparisons.
    #[arg(long)]
    pub turbo: bool,
    /// Simulate e-ink ghosting (screenshots then depend on history).
    #[arg(long)]
    pub ghosting: bool,
    /// Animate panel refreshes (X3, X4 Pro); off by default so frames are final images.
    #[arg(long)]
    pub animate: bool,
    /// Connect the emulated Wi-Fi to the host network (NAT).
    #[arg(long)]
    pub wifi: bool,
    /// Real-time clock start, as YYYY-MM-DDTHH:MM:SS, or `now`. Fixed by default so runs compare pixel for pixel.
    #[arg(long, default_value = "2026-01-01T09:00:00", value_name = "TIME", value_parser = rtc)]
    pub rtc: String,
    /// Directory with qemu-system-riscv32 / qemu-system-xtensa (default: the repository's build/ or EINK_QEMU_BIN).
    #[arg(long, value_name = "DIR")]
    pub qemu: Option<PathBuf>,
    /// Extra QEMU argument (repeatable), e.g. --qemu-arg=-d --qemu-arg=guest_errors.
    #[arg(long, value_name = "ARG", allow_hyphen_values = true)]
    pub qemu_arg: Vec<String>,
    /// Open a GDB server on 127.0.0.1:PORT.
    #[arg(long, value_name = "PORT")]
    pub gdb: Option<u16>,
    /// Do not keep every settled screen in frames/.
    #[arg(long)]
    pub no_frames: bool,
    /// Do not fail on guest crash lines (Guru Meditation, abort(), assert failed, ...).
    #[arg(long)]
    pub no_crash_check: bool,
    /// Another log line pattern that fails the run as a crash (repeatable).
    #[arg(long, value_name = "REGEX")]
    pub crash_pattern: Vec<String>,
    /// Print the firmware log on stderr as it comes.
    #[arg(long)]
    pub echo_log: bool,
    /// No progress lines on stderr (records still go to stdout).
    #[arg(long, short)]
    pub quiet: bool,
    /// Progress lines on stderr even when it is not a terminal.
    #[arg(long)]
    pub progress: bool,
    /// After the steps (or a failure), keep the emulator running for `eink-qemu do` until `eink-qemu stop`.
    #[arg(long)]
    pub hold: bool,
    /// End a held session after this long without a command, in seconds.
    #[arg(long, default_value_t = 1800, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=604_800))]
    pub hold_timeout: u64,
    /// Show the run in the desktop app's window (it stays open after the steps; close it to finish).
    #[arg(long)]
    pub gui: bool,
    #[arg(long, hide = true)]
    pub ready_file: Option<PathBuf>,
    /// The complete step list, one per line (start passes it to its background run).
    #[arg(long, hide = true)]
    pub steps_file: Option<PathBuf>,
    /// The token of the run directory `start` claimed for its background run.
    #[arg(long, hide = true)]
    pub claim: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = runtime.block_on(async move {
        match cli.command {
            Command::Run { firmware, steps, run } => run::run(firmware, steps, run).await,
            Command::Compare { base, candidate, steps, run, names, require_identical, sequential } => {
                compare::compare(base, candidate, steps, run, names, require_identical, sequential).await
            }
            Command::Start { firmware, steps, run } => control::start(firmware, steps, run).await,
            Command::Do { steps, session } => control::send(session, steps).await,
            Command::Stop { session } => control::stop(session).await,
            Command::Status { session } => control::status(session).await,
            Command::Report { dir } => match report::write(&dir) {
                Ok(path) => {
                    println!("{}", path.display());
                    0
                }
                Err(e) => {
                    eprintln!("eink-qemu report: {e}");
                    EXIT_USAGE
                }
            },
            Command::Steps => {
                print!("{}", eink_core::script::steps::STEP_HELP);
                0
            }
            Command::Flash { firmware, output, machine } => match eink_core::flash::make_flash(&firmware, machine.as_deref()) {
                Ok(flash) => match std::fs::write(&output, &flash.image) {
                    Ok(()) => {
                        println!("wrote {} ({} bytes, {} for {}, from {})", output.display(), flash.image.len(), flash.kind, flash.machine, firmware.display());
                        0
                    }
                    Err(e) => {
                        eprintln!("eink-qemu flash: cannot write {}: {e}", output.display());
                        EXIT_SETUP
                    }
                },
                Err(e) => {
                    eprintln!("eink-qemu flash: {e}");
                    EXIT_SETUP
                }
            },
            Command::Sd { command } => sdcmd::run(command),
        }
    });
    // Sessions end with the runtime; do not wait for stray tasks.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    ExitCode::from(code)
}
