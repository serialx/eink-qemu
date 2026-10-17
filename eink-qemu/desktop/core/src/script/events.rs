//! What a scripted run observes of its session: every console line with
//! the host time it arrived, display updates, and the session's state. The
//! lines are also written to the run directory as they come: log.txt as
//! the firmware printed them, console.jsonl with their times.

use crate::session::{Events, SessionInfo};
use serde_json::json;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

pub struct LogLine {
    /// Host seconds after the run started.
    pub t: f64,
    pub text: String,
}

#[derive(Default, Clone, Copy)]
pub struct ScreenState {
    /// Display messages so far; every update of the panel adds one.
    pub updates: u64,
    pub first_update: Option<Instant>,
    pub last_update: Option<Instant>,
}

pub struct RunEvents {
    pub started: Instant,
    lines: Mutex<Vec<LogLine>>,
    files: Mutex<Option<(BufWriter<File>, BufWriter<File>)>>,
    screen: Mutex<ScreenState>,
    state: Mutex<Option<SessionInfo>>,
    echo: bool,
}

impl RunEvents {
    /// Record into `dir` (log.txt, console.jsonl); with `echo`, also print
    /// each line to stderr.
    pub fn new(dir: &Path, echo: bool) -> Result<RunEvents, String> {
        let open = |name: &str| {
            File::create(dir.join(name)).map(BufWriter::new).map_err(|e| format!("cannot write {}: {e}", dir.join(name).display()))
        };
        Ok(RunEvents {
            started: Instant::now(),
            lines: Mutex::default(),
            files: Mutex::new(Some((open("log.txt")?, open("console.jsonl")?))),
            screen: Mutex::default(),
            state: Mutex::default(),
            echo,
        })
    }

    /// Host seconds since the run started.
    pub fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn line_count(&self) -> usize {
        self.lines.lock().unwrap().len()
    }

    /// Run `f` over the lines from `from` on.
    pub fn with_lines<T>(&self, from: usize, f: impl FnOnce(&[LogLine]) -> T) -> T {
        let lines = self.lines.lock().unwrap();
        f(&lines[from.min(lines.len())..])
    }

    /// As `with_lines`, also returning the index just past the lines `f`
    /// saw, taken under the same lock, for a cursor to continue from.
    pub fn with_lines_end<T>(&self, from: usize, f: impl FnOnce(&[LogLine]) -> T) -> (T, usize) {
        let lines = self.lines.lock().unwrap();
        (f(&lines[from.min(lines.len())..]), lines.len())
    }

    pub fn screen(&self) -> ScreenState {
        *self.screen.lock().unwrap()
    }

    pub fn state(&self) -> Option<SessionInfo> {
        self.state.lock().unwrap().clone()
    }

    /// Flush the log files (they are flushed after every batch anyway).
    pub fn flush(&self) {
        if let Some((log, jsonl)) = self.files.lock().unwrap().as_mut() {
            let _ = log.flush();
            let _ = jsonl.flush();
        }
    }
}

impl Events for RunEvents {
    fn session(&self, info: &SessionInfo) {
        *self.state.lock().unwrap() = Some(info.clone());
    }

    fn logs(&self, batch: Vec<String>) {
        let t = self.now();
        {
            let mut files = self.files.lock().unwrap();
            if let Some((log, jsonl)) = files.as_mut() {
                for line in &batch {
                    let _ = writeln!(log, "{line}");
                    let _ = writeln!(jsonl, "{}", json!({"t": (t * 1000.0).round() / 1000.0, "line": line}));
                }
                let _ = log.flush();
                let _ = jsonl.flush();
            }
        }
        if self.echo {
            let mut err = std::io::stderr().lock();
            for line in &batch {
                let _ = writeln!(err, "  | {line}");
            }
        }
        let mut lines = self.lines.lock().unwrap();
        lines.extend(batch.into_iter().map(|text| LogLine { t, text }));
    }

    fn display(&self, _message: &[u8]) {
        let now = Instant::now();
        let mut screen = self.screen.lock().unwrap();
        screen.updates += 1;
        screen.first_update.get_or_insert(now);
        screen.last_update = Some(now);
    }
}
