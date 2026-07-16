//! Recursive inotify watcher over the cgroup-v2 tree.
//!
//! Emits `CgroupEvent::{Added,Removed}` with absolute cgroup names. Newly
//! created directories are watched immediately and then scanned, closing the
//! create-race (a child created before its parent's watch was registered).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use inotify::{Inotify, WatchDescriptor, WatchMask};
use tokio::sync::mpsc;

use crate::HostError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupEvent {
    Added(String),
    Removed(String),
}

pub struct CgroupWatcher {
    root: PathBuf,
    inotify: Inotify,
    watches: HashMap<WatchDescriptor, PathBuf>,
}

impl CgroupWatcher {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, HostError> {
        let root = root.into();
        let inotify = Inotify::init().map_err(|e| HostError::io(&root, e))?;
        Ok(CgroupWatcher { root, inotify, watches: HashMap::new() })
    }

    fn cgroup_name(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let s = rel.to_string_lossy();
        if s.is_empty() { "/".to_string() } else { format!("/{s}") }
    }

    fn watch_dir(&mut self, dir: &Path, tx: &mpsc::UnboundedSender<CgroupEvent>, emit: bool) {
        let mask = WatchMask::CREATE | WatchMask::DELETE | WatchMask::MOVED_FROM | WatchMask::MOVED_TO | WatchMask::ONLYDIR;
        match self.inotify.watches().add(dir, mask) {
            Ok(wd) => {
                self.watches.insert(wd, dir.to_path_buf());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "inotify watch failed");
                return;
            }
        }
        if emit {
            let _ = tx.send(CgroupEvent::Added(self.cgroup_name(dir)));
        }
        // Scan children created before the watch existed.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    self.watch_dir(&entry.path(), tx, emit);
                }
            }
        }
    }

    /// Consumes the watcher: blocking loop feeding `tx`. Run it on a
    /// dedicated thread (`std::thread::spawn`); the receiver side is async.
    pub fn run(mut self, tx: mpsc::UnboundedSender<CgroupEvent>) {
        let root = self.root.clone();
        // Initial tree registration without Added events (the caller does its
        // own initial sweep); after this, every event is emitted.
        self.watch_dir(&root.clone(), &tx, false);

        let mut buf = [0u8; 16384];
        loop {
            let events = match self.inotify.read_events_blocking(&mut buf) {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!(error = %e, "inotify read failed");
                    return;
                }
            };
            for event in events {
                let Some(parent) = self.watches.get(&event.wd).cloned() else { continue };
                let Some(name) = event.name else { continue };
                let path = parent.join(name);
                use inotify::EventMask as M;
                if event.mask.intersects(M::CREATE | M::MOVED_TO) {
                    self.watch_dir(&path, &tx, true);
                } else if event.mask.intersects(M::DELETE | M::MOVED_FROM) {
                    let _ = tx.send(CgroupEvent::Removed(self.cgroup_name(&path)));
                    self.watches.retain(|_, p| p != &path);
                }
                if tx.is_closed() {
                    return;
                }
            }
        }
    }
}
