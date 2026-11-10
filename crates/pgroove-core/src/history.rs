//! Persisted query history and saved snippets.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, PoisonError, Weak};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::atomic;

/// Oldest entries are dropped beyond this many.
pub const MAX_HISTORY: usize = 500;

/// A snapshot to serialize and store; a newer one replaces it before it starts.
type Job = Box<dyn FnOnce(&Path) -> Result<()> + Send>;

/// One file's newest state, kept for the background thread.
struct Sink {
    path: PathBuf,
    pending: Mutex<Pending>,
    ready: Condvar,
    /// False when the writing thread could not start; writes then run inline.
    threaded: AtomicBool,
}

#[derive(Default)]
struct Pending {
    job: Option<Job>,
    /// Jobs queued so far, in order.
    queued: u64,
    /// The newest job that reached the disk.
    written: u64,
    /// The writer is finishing after its final flush.
    closing: bool,
}

impl Sink {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, Pending>) -> MutexGuard<'a, Pending> {
        self.ready
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one snapshot, then reports how much of the queue it covers.
    fn write(&self, job: Job, queued: u64) {
        if let Err(e) = job(&self.path) {
            log::warn!("could not save {}: {e:#}", self.path.display());
        }
        let mut g = self.lock();
        g.written = g.written.max(queued);
        drop(g);
        self.ready.notify_all();
    }

    /// Waits until everything queued so far is on disk.
    fn flush(&self) {
        let mut g = self.lock();
        let target = g.queued;
        while g.written < target && !g.closing {
            g = self.wait(g);
        }
    }
}

/// Every file written in the background, so the app can flush them all on exit.
fn registry() -> &'static Mutex<Vec<Weak<Sink>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<Sink>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn register(sink: &Arc<Sink>) {
    let mut g = registry().lock().unwrap_or_else(PoisonError::into_inner);
    g.retain(|w| w.strong_count() > 0);
    g.push(Arc::downgrade(sink));
}

/// Waits until every history and snippet file with pending changes is on disk.
///
/// Call this when the app's event loop ends, so a change made just before exit survives.
pub fn flush_all() {
    let live: Vec<Arc<Sink>> = {
        let mut g = registry().lock().unwrap_or_else(PoisonError::into_inner);
        g.retain(|w| w.strong_count() > 0);
        g.iter().filter_map(Weak::upgrade).collect()
    };
    for sink in &live {
        sink.flush();
    }
}

/// Coalescing, last-write-wins persistence of one file, off the calling thread.
///
/// Each queued job carries a complete snapshot, so dropping a job that has not started
/// is correct: the newest snapshot replaces it and one write covers the whole burst.
struct Writer {
    /// `None` for a file that is never written.
    sink: Option<Arc<Sink>>,
    started: Once,
}

impl Writer {
    fn new(path: PathBuf) -> Self {
        let sink = Some(path)
            .filter(|p| !p.as_os_str().is_empty())
            .map(|path| {
                let sink = Arc::new(Sink {
                    path,
                    pending: Mutex::new(Pending::default()),
                    ready: Condvar::new(),
                    threaded: AtomicBool::new(true),
                });
                register(&sink);
                sink
            });
        Self {
            sink,
            started: Once::new(),
        }
    }

    /// Queues `job`, replacing one that has not started yet.
    fn queue(&self, job: Job) {
        let Some(sink) = &self.sink else {
            return;
        };
        self.start(sink);
        let mut g = sink.lock();
        g.job = Some(job);
        g.queued += 1;
        let queued = g.queued;
        let threaded = sink.threaded.load(Ordering::Acquire);
        let inline = (!threaded).then(|| g.job.take()).flatten();
        drop(g);
        match inline {
            Some(job) => sink.write(job, queued),
            None => sink.ready.notify_one(),
        }
    }

    /// Waits until everything queued so far is on disk.
    fn flush(&self) {
        if let Some(sink) = &self.sink {
            sink.flush();
        }
    }

    /// Starts the writing thread on first use.
    fn start(&self, sink: &Arc<Sink>) {
        self.started.call_once(|| {
            let worker = sink.clone();
            let spawned = std::thread::Builder::new()
                .name("pgroove-persist".into())
                .spawn(move || {
                    loop {
                        let (job, queued) = {
                            let mut g = worker.lock();
                            loop {
                                if let Some(job) = g.job.take() {
                                    break (job, g.queued);
                                }
                                if g.closing {
                                    return;
                                }
                                g = worker.wait(g);
                            }
                        };
                        worker.write(job, queued);
                    }
                });
            match spawned {
                Ok(_) => sink.threaded.store(true, Ordering::Release),
                Err(e) => {
                    log::warn!("could not start the persistence thread: {e}");
                    sink.threaded.store(false, Ordering::Release);
                }
            }
        });
    }
}

impl Drop for Writer {
    /// Makes the last snapshot durable and stops the thread; cheap when idle.
    fn drop(&mut self) {
        let Some(sink) = &self.sink else {
            return;
        };
        sink.flush();
        let mut g = sink.lock();
        g.closing = true;
        drop(g);
        sink.ready.notify_all();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub millis: u64,
    pub ok: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default, rename = "entry")]
    entries: Vec<HistoryEntry>,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Describes how long ago `at` was, e.g. `just now`, `5 min ago`, `3 h ago`, `2 d ago`.
pub fn relative_time(at: u64, now: u64) -> String {
    let d = now.saturating_sub(at);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86400),
    }
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Most recent queries, newest last.
pub struct History {
    writer: Writer,
    entries: Vec<HistoryEntry>,
}

impl History {
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("pgroove").join("history.toml"))
    }

    /// A history that is never written to disk.
    pub fn in_memory() -> Self {
        Self {
            writer: Writer::new(PathBuf::new()),
            entries: Vec::new(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let entries = match read_text(&path)? {
            Some(t) => {
                toml::from_str::<HistoryFile>(&t)
                    .with_context(|| format!("parsing {}", path.display()))?
                    .entries
            }
            None => Vec::new(),
        };
        Ok(Self {
            writer: Writer::new(path),
            entries,
        })
    }

    /// Newest first.
    pub fn recent(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter().rev()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records a run; an identical statement run again moves to the newest position.
    ///
    /// The file is written in the background; a write failure is logged, not returned.
    pub fn push(&mut self, entry: HistoryEntry) {
        if entry.sql.trim().is_empty() {
            return;
        }
        self.entries.retain(|e| e.sql != entry.sql);
        self.entries.push(entry);
        if self.entries.len() > MAX_HISTORY {
            let extra = self.entries.len() - MAX_HISTORY;
            self.entries.drain(..extra);
        }
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    /// Waits until the entries pushed so far are on disk.
    pub fn flush(&self) {
        self.writer.flush();
    }

    fn save(&self) {
        let entries = self.entries.clone();
        self.writer.queue(Box::new(move |path| {
            let text = toml::to_string_pretty(&HistoryFile { entries })?;
            atomic::write(path, text.as_bytes())
        }));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub sql: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SnippetFile {
    #[serde(default, rename = "snippet")]
    snippets: Vec<Snippet>,
}

/// Saved named snippets.
pub struct Snippets {
    writer: Writer,
    items: Vec<Snippet>,
}

impl Snippets {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pgroove").join("snippets.toml"))
    }

    /// Snippets that are never written to disk.
    pub fn in_memory() -> Self {
        Self {
            writer: Writer::new(PathBuf::new()),
            items: Vec::new(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let items = match read_text(&path)? {
            Some(t) => {
                toml::from_str::<SnippetFile>(&t)
                    .with_context(|| format!("parsing {}", path.display()))?
                    .snippets
            }
            None => Vec::new(),
        };
        Ok(Self {
            writer: Writer::new(path),
            items,
        })
    }

    pub fn items(&self) -> &[Snippet] {
        &self.items
    }

    /// Adds a snippet, replacing one with the same name.
    ///
    /// The file is written in the background; a write failure is logged, not returned.
    pub fn save(&mut self, name: &str, sql: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "Enter a name");
        anyhow::ensure!(!sql.trim().is_empty(), "There is no SQL to save");
        let snippet = Snippet {
            name: name.to_string(),
            sql: sql.to_string(),
        };
        match self.items.iter().position(|s| s.name == name) {
            Some(i) => self.items[i] = snippet,
            None => self.items.push(snippet),
        }
        self.persist();
        Ok(())
    }

    pub fn remove(&mut self, name: &str) {
        self.items.retain(|s| s.name != name);
        self.persist();
    }

    /// Waits until the snippets saved so far are on disk.
    pub fn flush(&self) {
        self.writer.flush();
    }

    fn persist(&self) {
        let snippets = self.items.clone();
        self.writer.queue(Box::new(move |path| {
            let text = toml::to_string_pretty(&SnippetFile { snippets })?;
            atomic::write(path, text.as_bytes())
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, at: u64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            at,
            millis: 5,
            ok: true,
        }
    }

    fn part_of(path: &Path) -> PathBuf {
        atomic::sibling(path, ".part")
    }

    #[test]
    fn relative_times() {
        assert_eq!(relative_time(100, 100), "just now");
        assert_eq!(relative_time(0, 59), "just now");
        assert_eq!(relative_time(0, 300), "5 min ago");
        assert_eq!(relative_time(0, 7200), "2 h ago");
        assert_eq!(relative_time(0, 3 * 86400), "3 d ago");
        assert_eq!(relative_time(500, 100), "just now");
        assert!(now_secs() > 1_000_000_000);
    }

    #[test]
    fn history_persists_dedups_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/h.toml");
        let mut h = History::load(&path).unwrap();
        assert!(h.is_empty());
        h.push(entry("select 1", 1));
        h.flush();
        assert!(path.exists(), "the history file is written");
        assert!(!part_of(&path).exists(), "no part file is left");
        h.push(entry("select 2", 2));
        h.push(entry("select 1", 3));
        h.push(entry("   ", 4));
        h.flush();
        let sqls: Vec<_> = h.recent().map(|e| e.sql.as_str()).collect();
        assert_eq!(sqls, ["select 1", "select 2"]);
        let again = History::load(&path).unwrap();
        assert_eq!(again.len(), 2);
        assert_eq!(again.recent().next().unwrap().at, 3);

        let mut h = again;
        for i in 0..MAX_HISTORY + 5 {
            h.push(entry(&format!("select {i}"), i as u64));
        }
        assert_eq!(h.len(), MAX_HISTORY);
        assert_eq!(
            h.recent().next().unwrap().sql,
            format!("select {}", MAX_HISTORY + 4)
        );
        h.flush();
        assert_eq!(History::load(&path).unwrap().len(), MAX_HISTORY);
        h.clear();
        h.flush();
        assert!(History::load(&path).unwrap().is_empty());
    }

    #[test]
    fn burst_of_pushes_is_durable_after_a_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.toml");
        let mut h = History::load(&path).unwrap();
        for i in 0..200 {
            h.push(entry(&format!("select {i}"), i));
        }
        h.flush();
        let loaded = History::load(&path).unwrap();
        assert_eq!(loaded.len(), 200, "every pushed entry reached the file");
        assert_eq!(loaded.recent().next().unwrap().at, 199);
        assert!(!part_of(&path).exists(), "no part file is left");
    }

    #[test]
    fn a_flush_writes_exactly_the_memory_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.toml");
        let mut h = History::load(&path).unwrap();
        h.push(entry("select 1", 1));
        h.push(entry("select 2", 2));
        h.push(entry("select 1", 3));
        h.flush();
        let expected = toml::to_string_pretty(&HistoryFile {
            entries: h.entries.clone(),
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn clear_reaches_the_file_after_a_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.toml");
        let mut h = History::load(&path).unwrap();
        h.push(entry("select 1", 1));
        h.clear();
        h.flush();
        assert!(h.recent().next().is_none());
        assert!(History::load(&path).unwrap().is_empty());
    }

    #[test]
    fn a_burst_of_writes_coalesces_into_the_newest_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        let writer = Writer::new(path.clone());
        // Hold the writing thread inside its first job until the whole burst is queued,
        // so the burst has to coalesce instead of racing the thread.
        let (started, running) = std::sync::mpsc::channel::<()>();
        let (release, held) = std::sync::mpsc::channel::<()>();
        writer.queue(Box::new(move |_| {
            started.send(()).ok();
            held.recv().ok();
            Ok(())
        }));
        running.recv().unwrap();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for i in 0..50 {
            let writes = writes.clone();
            writer.queue(Box::new(move |path| {
                writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                atomic::write(path, format!("snapshot {i}").as_bytes())
            }));
        }
        release.send(()).ok();
        writer.flush();
        assert_eq!(
            writes.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the burst costs one write, not one per push"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "snapshot 49");
        assert!(!part_of(&path).exists(), "no part file is left");
    }

    #[test]
    fn flush_all_covers_a_pending_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.toml");
        let mut h = History::load(&path).unwrap();
        h.push(entry("select 1", 1));
        flush_all();
        assert_eq!(History::load(&path).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_files_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.toml");
        std::fs::write(&path, "entry = 3").unwrap();
        assert!(History::load(&path).is_err());
        std::fs::write(&path, "snippet = 3").unwrap();
        assert!(Snippets::load(&path).is_err());
        let _ = (History::default_path(), Snippets::default_path());
        let mut h = History::in_memory();
        h.push(entry("select 1", 1));
        h.flush();
        assert_eq!(h.len(), 1);
        let mut s = Snippets::in_memory();
        s.save("a", "select 1").unwrap();
        s.flush();
        assert_eq!(s.items().len(), 1);
    }

    #[test]
    fn snippets_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        let mut s = Snippets::load(&path).unwrap();
        assert!(s.items().is_empty());
        s.save(" top orders ", "select * from orders limit 10")
            .unwrap();
        s.save("top orders", "select 2").unwrap();
        s.save("other", "select 3").unwrap();
        s.flush();
        assert!(!part_of(&path).exists(), "no part file is left");
        assert!(s.save("  ", "x").is_err());
        assert!(s.save("n", "  ").is_err());
        let loaded = Snippets::load(&path).unwrap();
        assert_eq!(loaded.items().len(), 2);
        assert_eq!(loaded.items()[0].sql, "select 2");
        let mut loaded = loaded;
        loaded.remove("top orders");
        loaded.flush();
        assert_eq!(Snippets::load(&path).unwrap().items()[0].name, "other");
    }
}
