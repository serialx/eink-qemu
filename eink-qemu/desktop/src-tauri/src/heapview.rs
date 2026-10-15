//! The heap view's state, shared by the Heap tab and the heap window: the
//! ELF, its symbols and allocation sites, the current run's allocation log,
//! the free-memory history and the latest snapshot. Its answers have the
//! shape of heapview.py's endpoints, which eink-qemu/heapview.js reads.

use crate::heap::Elf;
use crate::session::{AppError, Session};
use crate::trace::{stack_frames, Sites, Symbols, Trace};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Snapshots this recent are shared between the tab and the window.
const REUSE: Duration = Duration::from_millis(800);
/// History points kept (15 minutes at one a second).
const HISTORY: usize = 900;

#[derive(Default)]
pub struct HeapView {
    elf: Option<Arc<Elf>>,
    symbols: Option<Symbols>,
    sites: Sites,
    /// The run whose log `trace` reads.
    trace: Option<(u64, Trace)>,
    /// Log generations handed out, so the window starts over on a new one.
    gens: u64,
    history: VecDeque<Value>,
    /// The run the history and the last snapshot belong to.
    run: u64,
    last: Option<(Instant, Value)>,
}

/// Read an ELF off the async runtime.
pub async fn read_elf(path: PathBuf) -> Result<Elf, AppError> {
    tokio::task::spawn_blocking(move || Elf::load(&path))
        .await
        .unwrap_or_else(|_| Err("Reading the ELF panicked.".into()))
        .map_err(|message| AppError::new("EINVAL", message))
}

impl HeapView {
    /// Use `elf` (None to forget it): symbols, sites and the log start over.
    pub fn set_elf(&mut self, elf: Option<Arc<Elf>>) {
        self.elf = elf;
        self.symbols = None;
        self.sites = Sites::default();
        self.trace = None;
        self.last = None;
    }

    /// The ELF at `path`, read again when it was rebuilt.
    pub async fn elf(&mut self, path: &Path) -> Result<Arc<Elf>, AppError> {
        if let Some(elf) = self.elf.as_ref().filter(|elf| elf.path == path && !elf.stale()) {
            return Ok(elf.clone());
        }
        let elf = Arc::new(read_elf(path.to_path_buf()).await?);
        self.set_elf(Some(elf.clone()));
        Ok(elf)
    }

    /// Read what the plugin appended, symbolizing new stacks.
    fn update_trace(&mut self) {
        let (Some((_, trace)), Some(elf)) = (self.trace.as_mut(), self.elf.as_ref()) else { return };
        let symbols = self.symbols.get_or_insert_with(|| Symbols::new(&elf.path));
        let sites = &mut self.sites;
        // Symbolizing parses DWARF: keep it off the async workers' backs.
        tokio::task::block_in_place(|| trace.update(|stack| sites.of(symbols, stack)));
    }

    /// The running firmware's heaps, as heapview.py's /snapshot.
    pub async fn snapshot(&mut self, session: &Arc<Session>, path: &Path) -> Result<Value, AppError> {
        if let Some((at, snap)) = &self.last {
            if at.elapsed() < REUSE && session.info().run == self.run {
                return Ok(snap.clone());
            }
        }
        let elf = self.elf(path).await?;
        let started = Instant::now();
        let read = session.heap_snapshot(&elf).await?;
        let took = started.elapsed();
        if read.run != self.run {
            self.run = read.run;
            self.history.clear();
            self.trace = None;
        }
        let trace_note = match &read.trace {
            Ok(log) => {
                if self.trace.as_ref().is_none_or(|(run, _)| *run != read.run) {
                    self.gens += 1;
                    self.trace = Some((read.run, Trace::new(log.clone(), self.gens)));
                }
                None
            }
            Err(reason) => Some(reason.clone()),
        };
        self.update_trace();
        let trace = self.trace.as_ref().map(|(_, trace)| trace);
        let traced = trace.is_some_and(Trace::traced);
        let mut heaps = Vec::with_capacity(read.snapshot.heaps.len());
        for heap in &read.snapshot.heaps {
            let mut value = serde_json::to_value(heap).unwrap_or_default();
            if let (true, Some(trace)) = (traced, trace) {
                let (sites, stacks, born) = trace.label(heap.start, &heap.blocks);
                value["sites"] = json!(sites);
                value["stk"] = json!(stacks);
                value["ev"] = json!(born);
            }
            heaps.push(value);
        }
        let time = read.snapshot.time as f64 / 1000.0;
        let snap = json!({
            "time": time,
            "elf": elf.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            "running": true,
            "heaps": heaps,
            "traced": traced,
            "traceGen": trace.map_or(0, |t| t.gen),
            "traceEvents": trace.map_or(0, Trace::events_len),
            "traceFormat": trace.is_none_or(|t| t.format_ok),
            "traceNote": trace_note,
            "interval": 1,
            "missing": read.snapshot.missing,
            "took": took.as_secs_f64() * 1000.0,
        });
        let point: serde_json::Map<String, Value> = read.snapshot.heaps.iter()
            .map(|h| (format!("{:x}", h.start), json!([h.info_free, h.largest, h.info_min_free])))
            .collect();
        self.history.push_back(json!({"t": time, "h": point}));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        if let Some(path) = std::env::var_os("EINK_QEMU_HEAP_DUMP") {
            // Tests: every snapshot, with the log's events, as JSON.
            let events = self.trace.as_ref().map(|(_, trace)| trace.events(trace.gen, 0, 0, 0, &self.sites.names));
            let _ = std::fs::write(path, json!({"snapshot": snap, "events": events}).to_string());
        }
        self.last = Some((Instant::now(), snap.clone()));
        Ok(snap)
    }

    /// The current run's free-memory history, as heapview.py's /history.
    pub fn history(&self, session: &Session) -> Value {
        if session.info().run != self.run {
            return json!([]);
        }
        json!(self.history)
    }

    /// Events from `start`, as heapview.py's /events; null without a log.
    pub fn events(&mut self, gen: u64, start: usize, stacks: usize, sites: usize) -> Value {
        self.update_trace();
        match &self.trace {
            Some((_, trace)) => trace.events(gen, start, stacks, sites, &self.sites.names),
            None => Value::Null,
        }
    }

    /// A recorded stack, symbolized, as heapview.py's /stack; null if unknown.
    pub fn stack(&mut self, id: usize) -> Value {
        let (Some((_, trace)), Some(elf)) = (self.trace.as_ref(), self.elf.as_ref()) else { return Value::Null };
        let Some(addrs) = trace.stack(id) else { return Value::Null };
        let symbols = self.symbols.get_or_insert_with(|| Symbols::new(&elf.path));
        json!(stack_frames(symbols, addrs))
    }
}
