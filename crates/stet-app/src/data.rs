//! The data files that data refs name, peeked at off the UI thread and kept
//! until the file changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};
use stet_data::Peek;

/// Rows a chip's preview shows.
pub const PREVIEW_ROWS: usize = 50;
/// Rows a chart reads from a file.
pub const CHART_ROWS: usize = 100_000;
/// A file is looked at again for changes at most this often.
const RECHECK: Duration = Duration::from_secs(1);

/// What was read: the file, its table, and how many rows.
type Key = (PathBuf, Option<String>, usize);

enum State {
    Loading,
    Ready(Arc<Peek>),
    Failed(String),
}

struct Entry {
    state: State,
    /// The file's modification time when it was read.
    stamp: Option<SystemTime>,
    checked: Instant,
}

pub enum Look<'a> {
    Loading,
    Ready(&'a Arc<Peek>),
    Failed(&'a str),
}

pub struct Data {
    entries: HashMap<Key, Entry>,
    sender: Sender<(Key, Result<Peek, String>)>,
    receiver: Receiver<(Key, Result<Peek, String>)>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Read on the calling thread (screenshots).
    pub blocking: bool,
}

/// Where a data ref's path points: beside the document, or under the
/// current directory for a document not yet saved.
pub fn resolve(path: &str, document: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(path.strip_prefix("file://").unwrap_or(path));
    if path.is_absolute() {
        return path;
    }
    match document.and_then(Path::parent) {
        Some(dir) => dir.join(path),
        None => std::env::current_dir().unwrap_or_default().join(path),
    }
}

fn stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

impl Data {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Data {
        let (sender, receiver) = channel();
        Data {
            entries: HashMap::new(),
            sender,
            receiver,
            wake,
            blocking: false,
        }
    }

    /// The first `rows` rows of `path` (its `table`, for a database), once
    /// read. A file that changed since is read again.
    pub fn get(&mut self, path: &Path, table: Option<&str>, rows: usize) -> Look<'_> {
        while let Ok((key, peek)) = self.receiver.try_recv() {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.state = match peek {
                    Ok(peek) => State::Ready(Arc::new(peek)),
                    Err(err) => State::Failed(err),
                };
            }
        }
        let key = (path.to_path_buf(), table.map(str::to_string), rows);
        let now = Instant::now();
        let stale = match self.entries.get_mut(&key) {
            None => true,
            Some(entry) if matches!(entry.state, State::Loading) => false,
            Some(entry) if now.duration_since(entry.checked) < RECHECK => false,
            Some(entry) => {
                entry.checked = now;
                stamp(path) != entry.stamp
            }
        };
        if stale {
            self.entries.insert(
                key.clone(),
                Entry {
                    state: State::Loading,
                    stamp: stamp(path),
                    checked: now,
                },
            );
            self.start(key.clone());
        }
        match &self.entries[&key].state {
            State::Loading => Look::Loading,
            State::Ready(peek) => Look::Ready(peek),
            State::Failed(err) => Look::Failed(err),
        }
    }

    fn start(&mut self, key: Key) {
        let (sender, wake) = (self.sender.clone(), self.wake.clone());
        let work = move || {
            let peek = stet_data::peek(&key.0, key.1.as_deref(), key.2);
            let _ = sender.send((key, peek));
            wake();
        };
        if self.blocking {
            work();
            // Taken in at once, so the frame drawing now sees it.
            while let Ok((key, peek)) = self.receiver.try_recv() {
                if let Some(entry) = self.entries.get_mut(&key) {
                    entry.state = match peek {
                        Ok(peek) => State::Ready(Arc::new(peek)),
                        Err(err) => State::Failed(err),
                    };
                }
            }
        } else {
            std::thread::spawn(work);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_read_again_when_they_change() {
        let dir = std::env::temp_dir().join(format!("stet-app-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.csv");
        std::fs::write(&path, "x\n1\n").unwrap();
        let mut data = Data::new(Arc::new(|| {}));
        data.blocking = true;
        let rows = |data: &mut Data| match data.get(&path, None, 10) {
            Look::Ready(peek) => peek.total,
            _ => None,
        };
        assert_eq!(rows(&mut data), Some(1));
        std::fs::write(&path, "x\n1\n2\n").unwrap();
        // The modification time can be coarse: make sure it moves.
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        data.entries.values_mut().for_each(|entry| entry.checked -= RECHECK);
        assert_eq!(rows(&mut data), Some(2));
        assert!(resolve("d/a.csv", Some(Path::new("/n/doc.md"))).ends_with("n/d/a.csv"));
        assert!(matches!(data.get(&dir.join("none.csv"), None, 1), Look::Failed(_)));
    }
}
