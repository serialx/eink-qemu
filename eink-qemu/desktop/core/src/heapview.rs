//! The heap view's state, shared by the Heap tab and the heap window: the
//! ELF, its symbols and allocation sites, the current run's allocation log,
//! the free-memory history with a snapshot for each of its points (the Heap
//! tab's time machine), and the latest snapshot. Its answers have the shape
//! of the earlier heapview.py's endpoints, which the Heap tab and window read.

use crate::heap::Elf;
use crate::session::{AppError, Session};
use crate::trace::{stack_frames, Sites, Symbols, Trace};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
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
    /// Each history point's snapshot as deflated JSON, by its time.
    frames: VecDeque<(f64, Vec<u8>)>,
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
        let _ = self.update_trace_until(None);
    }

    fn update_trace_until(&mut self, end: Option<u64>) -> Result<(), String> {
        let (_, trace) = self.trace.as_mut().ok_or("no allocation log for the current run")?;
        let Some(elf) = self.elf.as_ref() else {
            return trace.update_until(end, |_| -1);
        };
        let symbols = self.symbols.get_or_insert_with(|| Symbols::new(&elf.path));
        let sites = &mut self.sites;
        // Symbolizing parses DWARF: keep it off the async workers' backs.
        tokio::task::block_in_place(|| trace.update_until(end, |stack| sites.of(symbols, stack)))
    }

    /// The running firmware's heaps, as heapview.py's /snapshot. The ELF
    /// finds the heap list and labels blocks; without one, or when it is not
    /// the running firmware's, the list is found by a scan and `elfNote`
    /// says why the blocks are not labelled.
    pub async fn snapshot(&mut self, session: &Arc<Session>, path: Option<&Path>) -> Result<Value, AppError> {
        if let Some((at, snap)) = &self.last {
            if at.elapsed() < REUSE && session.info().run == self.run {
                return Ok(snap.clone());
            }
        }
        let (mut elf, mut elf_note) = match path {
            Some(path) => match self.elf(path).await {
                Ok(elf) => (Some(elf), None),
                Err(error) => (None, Some(error.message)),
            },
            None => (None, None),
        };
        let started = Instant::now();
        let read = match session.heap_snapshot(elf.as_deref()).await {
            Err(error) if error.code == "EMISMATCH" => {
                elf = None;
                elf_note = Some(error.message);
                session.heap_snapshot(None).await?
            }
            read => read?,
        };
        let took = started.elapsed();
        if read.run != self.run {
            self.run = read.run;
            self.history.clear();
            self.frames.clear();
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
        if elf.is_some() {
            self.update_trace();
        }
        let trace = self.trace.as_ref().map(|(_, trace)| trace).filter(|_| elf.is_some());
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
            "elf": elf.as_ref().and_then(|elf| elf.path.file_name()).map(|n| n.to_string_lossy().into_owned()),
            "elfNote": elf_note,
            "running": true,
            "run": read.run,
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
        self.frames.push_back((time, deflate(&snap)));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        while self.frames.len() > HISTORY {
            self.frames.pop_front();
        }
        if let Some(path) = std::env::var_os("EINK_QEMU_HEAP_DUMP") {
            // Tests: every snapshot, with the log's events, as JSON.
            let events = self.trace.as_ref().map(|(_, trace)| trace.events(trace.gen, 0, 0, 0, &self.sites.names));
            let _ = std::fs::write(path, json!({"snapshot": snap, "events": events}).to_string());
        }
        self.last = Some((Instant::now(), snap.clone()));
        Ok(snap)
    }

    /// Follow run `run`'s allocation log at `log` (as a snapshot of that run
    /// would), so a window can be read before any snapshot.
    pub fn use_trace(&mut self, run: u64, log: PathBuf) {
        if self.trace.as_ref().is_none_or(|(r, _)| *r != run) {
            self.gens += 1;
            self.trace = Some((run, Trace::new(log, self.gens)));
        }
    }

    /// Forget the last snapshot, so the next one is read afresh even within
    /// the reuse interval (a scripted run reads heaps right after input).
    pub fn invalidate(&mut self) {
        self.last = None;
    }

    /// Activity between file offsets captured with the guest stopped, by
    /// site with names, largest kept bytes first. Parse only through `to`.
    pub fn trace_window(&mut self, from: u64, to: u64) -> Result<Value, String> {
        self.update_trace_until(Some(to))?;
        let (_, trace) = self.trace.as_ref().ok_or("no allocation log for the current run")?;
        let sites = trace.window(from, to)?;
        let total = |f: fn(&crate::trace::SiteWindow) -> u64| sites.iter().map(f).sum::<u64>();
        let named: Vec<Value> = sites.iter().map(|w| {
            let mut v = serde_json::to_value(w).unwrap_or_default();
            let name = usize::try_from(w.site).ok().and_then(|i| self.sites.names.get(i)).cloned();
            v["fn"] = name.as_ref().and_then(|n| n.get("fn")).cloned().unwrap_or(json!("??"));
            v["loc"] = name.as_ref().and_then(|n| n.get("loc")).cloned().unwrap_or(json!(""));
            v
        }).collect();
        Ok(json!({
            "allocs": total(|w| w.allocs),
            "alloc_bytes": total(|w| w.alloc_bytes),
            "freed": total(|w| w.freed),
            "kept": total(|w| w.kept),
            "kept_bytes": total(|w| w.kept_bytes),
            "older_freed": total(|w| w.older_freed),
            "older_freed_bytes": total(|w| w.older_freed_bytes),
            "events": trace.events_len(),
            "sites": named,
        }))
    }

    /// The current run's free-memory history, as heapview.py's /history.
    pub fn history(&self, session: &Session) -> Value {
        if session.info().run != self.run {
            return json!([]);
        }
        json!(self.history)
    }

    /// The snapshot kept for the history point nearest `time`, and the one
    /// before it, so the tab can show what changed; null for another run.
    pub fn snapshot_at(&self, session: &Session, time: f64) -> Value {
        if session.info().run != self.run || self.frames.is_empty() {
            return Value::Null;
        }
        let after = self.frames.partition_point(|(t, _)| *t < time);
        let i = match after {
            0 => 0,
            n if n == self.frames.len() => n - 1,
            n => if time - self.frames[n - 1].0 <= self.frames[n].0 - time { n - 1 } else { n },
        };
        json!({
            "snapshot": inflate(&self.frames[i].1),
            "prev": i.checked_sub(1).map(|p| inflate(&self.frames[p].1)),
        })
    }

    /// Events from `start`, as heapview.py's /events; null without a log.
    pub fn events(&mut self, gen: u64, start: usize, stacks: usize, sites: usize) -> Value {
        self.update_trace();
        match &self.trace {
            Some((_, trace)) => trace.events(gen, start, stacks, sites, &self.sites.names),
            None => Value::Null,
        }
    }

    /// Allocation sites from `from` on, as {fn, loc}; a block's site indexes them.
    pub fn sites(&self, from: usize) -> Value {
        json!(self.sites.names.get(from..).unwrap_or_default())
    }

    /// A recorded stack, symbolized, as heapview.py's /stack; null if unknown.
    pub fn stack(&mut self, id: usize) -> Value {
        let (Some((_, trace)), Some(elf)) = (self.trace.as_ref(), self.elf.as_ref()) else { return Value::Null };
        let Some(addrs) = trace.stack(id) else { return Value::Null };
        let symbols = self.symbols.get_or_insert_with(|| Symbols::new(&elf.path));
        json!(stack_frames(symbols, addrs))
    }
}

/// A kept snapshot: JSON, deflated (a DRAM heap's block list repeats a lot).
fn deflate(snap: &Value) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = encoder.write_all(&serde_json::to_vec(snap).unwrap_or_default());
    encoder.finish().unwrap_or_default()
}

fn inflate(bytes: &[u8]) -> Value {
    let mut json = Vec::new();
    let _ = flate2::read::DeflateDecoder::new(bytes).read_to_end(&mut json);
    serde_json::from_slice(&json).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_snapshots_deflated() {
        let snap = json!({"time": 1.5, "heaps": [{"blocks": (0..4000).collect::<Vec<u32>>()}]});
        let bytes = deflate(&snap);
        assert!(bytes.len() < serde_json::to_vec(&snap).unwrap().len() / 2);
        assert_eq!(inflate(&bytes), snap);
    }
}
