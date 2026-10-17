//! What a scripted run observes of its session: every console line with
//! the host time it arrived, display updates, and the session's state. The
//! lines are also written to the run directory as they come: log.txt as
//! the firmware printed them, console.jsonl with their times.

use crate::session::{Events, SessionInfo};
use serde_json::json;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
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
    dir: PathBuf,
    /// The first failure to write log.txt or console.jsonl.
    write_error: Mutex<Option<String>>,
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
            dir: dir.to_path_buf(),
            write_error: Mutex::default(),
        })
    }

    /// The first failure to write the log files, if any: the saved
    /// firmware log is incomplete.
    pub fn write_error(&self) -> Option<String> {
        self.write_error.lock().unwrap().clone()
    }

    fn note_write(&self, written: std::io::Result<()>) {
        if let Err(e) = written {
            self.write_error.lock().unwrap().get_or_insert_with(|| {
                format!("cannot write log.txt or console.jsonl in {}: {e}; the saved firmware log is incomplete", self.dir.display())
            });
        }
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
            self.note_write(log.flush());
            self.note_write(jsonl.flush());
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
                    self.note_write(writeln!(log, "{line}"));
                    self.note_write(writeln!(jsonl, "{}", json!({"t": (t * 1000.0).round() / 1000.0, "line": line})));
                }
                self.note_write(log.flush());
                self.note_write(jsonl.flush());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_log_write_is_kept_and_the_line_too() {
        let dir = tempfile::tempdir().unwrap();
        let events = RunEvents::new(dir.path(), false).unwrap();
        events.logs(vec!["I (10) boot: first".into()]);
        assert_eq!(events.write_error(), None);
        // Files that cannot be written, as on a full disk.
        let read_only = || BufWriter::new(File::open(dir.path().join("log.txt")).unwrap());
        *events.files.lock().unwrap() = Some((read_only(), read_only()));
        events.logs(vec!["I (20) boot: second".into()]);
        let error = events.write_error().expect("the failed write");
        assert!(error.contains("the saved firmware log is incomplete"), "{error}");
        // The run still sees every line.
        assert_eq!(events.line_count(), 2);
    }
}
