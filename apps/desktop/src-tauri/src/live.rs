//! Live change detection: while the app runs, connected drives and catalogued folders are
//! watched with the OS's change notifications (ReadDirectoryChangesW on Windows, FSEvents
//! on macOS, inotify on Linux) and what changes is applied to the catalogue in place.
//!
//! - Events are gathered per volume and applied once things go quiet for two seconds
//!   (at most ten seconds after the first one, so a busy disk still updates).
//! - Changes made while Storecat was closed are left to the rescan when a drive is
//!   plugged in (see `watcher.rs`).
//! - When the OS says it dropped events, the folder concerned is compared with the
//!   catalogue again; when that's the whole drive, it is rescanned in the background.
//!   Windows doesn't always say: a burst big enough to overflow its buffer is treated as
//!   if it had.
//! - Network shares are not watched: change notifications over SMB/NFS are not reliable
//!   (other clients' changes often never arrive), so they keep relying on rescans.
//!
//! With nothing changing the thread sleeps; every five seconds it checks which volumes
//! are connected and whether the setting is still on.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use storecat_core::{settings, Catalog, Change, Error, MediaType, VolumeId, VolumeSource};
use tauri::{AppHandle, Emitter};

/// Apply once no new event has arrived for this long...
const QUIET: Duration = Duration::from_secs(2);
/// ...or this long after the first event of a batch, whichever comes first.
const MAX_WAIT: Duration = Duration::from_secs(10);
/// How often the set of watched volumes is brought in line with what is connected.
const SYNC_EVERY: Duration = Duration::from_secs(5);
/// A volume that could not be watched is tried again after this long.
const RETRY_WATCH: Duration = Duration::from_secs(300);
/// New files are enriched in one go, at most this often per volume.
const ENRICH_EVERY: Duration = Duration::from_secs(60);
/// On Windows a flood this large and fast may have overflowed the change buffer unnoticed
/// (the `notify` crate doesn't pass the overflow on). Busy system drives easily see a few
/// thousand changes in a few seconds from browsers, builds and installers, which the buffer
/// keeps up with; an overflow takes hundreds landing within milliseconds.
const FLOOD_EVENTS: usize = 5_000;
const FLOOD_PER_SEC: f64 = 1_000.0;
/// A whole drive is rescanned for live changes at most this long after its last scan; a
/// request in between waits (changes keep being applied meanwhile). A busy system drive
/// can overflow the change buffer often, and each rescan reads every file entry.
const FULL_RESCAN_GAP_SECS: i64 = 10 * 60;
/// Failed applies are retried this many times before the volume is rescanned instead.
const MAX_FAILURES: u32 = 3;

pub fn start(app: AppHandle, catalog: Arc<Catalog>) {
    std::thread::Builder::new()
        .name("storecat-live".into())
        .spawn(move || Live::new(app, catalog).run())
        .expect("spawn live watcher thread");
}

struct Watched {
    root: PathBuf,
    /// The root as the OS reports it, when that differs (symlinked folders on macOS).
    real_root: Option<PathBuf>,
}

#[derive(Default)]
struct Pending {
    renames: Vec<(PathBuf, PathBuf)>,
    rescans: BTreeSet<PathBuf>,
    paths: BTreeSet<PathBuf>,
    events: usize,
    first: Option<Instant>,
    last: Option<Instant>,
    failures: u32,
}

impl Pending {
    fn touch(&mut self, now: Instant) {
        self.events += 1;
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn due(&self, now: Instant) -> bool {
        match (self.first, self.last) {
            (Some(first), Some(last)) => {
                now.duration_since(last) >= QUIET || now.duration_since(first) >= MAX_WAIT
            }
            _ => false,
        }
    }

    fn is_empty(&self) -> bool {
        self.renames.is_empty() && self.rescans.is_empty() && self.paths.is_empty()
    }

    /// Whether the changes arrived in a flood that may have overflowed the OS's buffer.
    fn flooded(&self) -> bool {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return false;
        };
        let secs = last.duration_since(first).as_secs_f64().max(1.0);
        self.events >= FLOOD_EVENTS && self.events as f64 / secs >= FLOOD_PER_SEC
    }
}

struct Live {
    app: AppHandle,
    catalog: Arc<Catalog>,
    tx: std::sync::mpsc::Sender<notify::Result<Event>>,
    rx: Receiver<notify::Result<Event>>,
    watcher: Option<RecommendedWatcher>,
    watched: HashMap<VolumeId, Watched>,
    failed: HashMap<VolumeId, (String, Instant)>,
    pending: HashMap<VolumeId, Pending>,
    /// A rename's old name, waiting for its new name (Windows reports them apart).
    rename_from: Option<PathBuf>,
    rescan_full: HashSet<VolumeId>,
    enrich_wanted: HashSet<VolumeId>,
    last_enrich: HashMap<VolumeId, Instant>,
    last_sync: Option<Instant>,
}

impl Live {
    fn new(app: AppHandle, catalog: Arc<Catalog>) -> Self {
        let (tx, rx) = channel();
        Self {
            app,
            catalog,
            tx,
            rx,
            watcher: None,
            watched: HashMap::new(),
            failed: HashMap::new(),
            pending: HashMap::new(),
            rename_from: None,
            rescan_full: HashSet::new(),
            enrich_wanted: HashSet::new(),
            last_enrich: HashMap::new(),
            last_sync: None,
        }
    }

    fn run(mut self) {
        loop {
            let wait = if self.pending.is_empty() && self.enrich_wanted.is_empty() {
                SYNC_EVERY
            } else {
                Duration::from_millis(500)
            };
            match self.rx.recv_timeout(wait) {
                Ok(ev) => {
                    self.on_event(ev);
                    // Drain whatever else is queued before doing any work.
                    while let Ok(ev) = self.rx.try_recv() {
                        self.on_event(ev);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.flush_rename();
            let now = Instant::now();
            if self
                .last_sync
                .map(|t| now.duration_since(t) >= SYNC_EVERY)
                .unwrap_or(true)
            {
                self.last_sync = Some(now);
                self.sync();
            }
            self.apply_due(now);
            self.start_rescans();
            self.start_enrichment(now);
        }
    }

    // ----- which volumes to watch --------------------------------------------------

    fn sync(&mut self) {
        let enabled = self.catalog.setting_enabled(settings::LIVE_WATCH, true);
        let mut want: HashMap<VolumeId, PathBuf> = HashMap::new();
        if enabled {
            match self.catalog.volumes() {
                Ok(vols) => {
                    for v in vols {
                        let Some(root) = v.mount_path.clone() else {
                            continue;
                        };
                        if !v.online
                            || v.last_scan_at.is_none()
                            || matches!(v.media_type, MediaType::Network | MediaType::Optical)
                        {
                            continue;
                        }
                        if v.source == VolumeSource::Folder || cfg!(windows) {
                            // Mapped network drives and shares added as folders.
                            if storecat_platform::is_remote_path(&root) {
                                continue;
                            }
                        }
                        want.insert(v.id, PathBuf::from(root));
                    }
                }
                Err(e) => {
                    log::debug!("live: could not list volumes: {e}");
                    return;
                }
            }
        }

        let stale: Vec<VolumeId> = self
            .watched
            .iter()
            .filter(|(id, w)| want.get(id) != Some(&w.root))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(w) = self.watched.remove(&id) {
                if let Some(watcher) = self.watcher.as_mut() {
                    let _ = watcher.unwatch(&w.root);
                }
                log::info!("live: stopped watching {}", w.root.display());
            }
            if !want.contains_key(&id) {
                self.pending.remove(&id);
            }
        }
        self.failed.retain(|id, (root, at)| {
            want.get(id).map(|r| r.as_os_str()) == Some(Path::new(root).as_os_str())
                && at.elapsed() < RETRY_WATCH
        });
        if want.is_empty() {
            if self.watched.is_empty() && self.watcher.is_some() {
                self.watcher = None;
                self.pending.clear();
                self.rename_from = None;
            }
            return;
        }
        for (id, root) in want {
            if self.watched.contains_key(&id) || self.failed.contains_key(&id) {
                continue;
            }
            match self.watch(&root) {
                Ok(()) => {
                    log::info!("live: watching {}", root.display());
                    let real_root = std::fs::canonicalize(&root)
                        .ok()
                        .map(|p| strip_verbatim(&p))
                        .filter(|p| *p != root);
                    self.watched.insert(id, Watched { root, real_root });
                }
                Err(e) => {
                    log::warn!("live: cannot watch {}: {e}", root.display());
                    self.failed
                        .insert(id, (root.to_string_lossy().into_owned(), Instant::now()));
                }
            }
        }
    }

    fn watch(&mut self, root: &Path) -> notify::Result<()> {
        if self.watcher.is_none() {
            let tx = self.tx.clone();
            self.watcher = Some(notify::recommended_watcher(move |res| {
                let _ = tx.send(res);
            })?);
        }
        self.watcher
            .as_mut()
            .expect("created above")
            .watch(root, RecursiveMode::Recursive)
    }

    /// Watch a volume afresh, in case the OS dropped the watch after an overflow.
    fn rearm(&mut self, id: VolumeId) {
        let Some(root) = self.watched.get(&id).map(|w| w.root.clone()) else {
            return;
        };
        if let Some(watcher) = self.watcher.as_mut() {
            let _ = watcher.unwatch(&root);
            if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
                log::warn!("live: cannot watch {} again: {e}", root.display());
                self.watched.remove(&id);
                self.failed
                    .insert(id, (root.to_string_lossy().into_owned(), Instant::now()));
            }
        }
    }

    // ----- events --------------------------------------------------------------------

    /// The watched volumes a path belongs to (more than one when catalogued folders nest),
    /// with the path spelled from each volume's root.
    fn owners(&self, path: &Path) -> Vec<(VolumeId, PathBuf)> {
        let mut out = Vec::new();
        for (id, w) in &self.watched {
            if self.catalog.live_rel_path(&w.root, path).is_some() {
                out.push((*id, path.to_path_buf()));
            } else if let Some(real) = &w.real_root {
                if let Ok(rest) = path.strip_prefix(real) {
                    let p = w.root.join(rest);
                    if self.catalog.live_rel_path(&w.root, &p).is_some() {
                        out.push((*id, p));
                    }
                }
            }
        }
        out
    }

    fn on_event(&mut self, ev: notify::Result<Event>) {
        let now = Instant::now();
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                log::warn!("live: watch error: {e}");
                if e.paths.is_empty() {
                    for id in self.watched.keys().copied().collect::<Vec<_>>() {
                        self.rescan_full.insert(id);
                    }
                } else {
                    for p in &e.paths {
                        for (id, p) in self.owners(p) {
                            let pend = self.pending.entry(id).or_default();
                            pend.rescans.insert(p);
                            pend.touch(now);
                        }
                    }
                }
                return;
            }
        };
        if ev.need_rescan() {
            if ev.paths.is_empty() {
                log::info!("live: the OS dropped change events; rescanning watched drives");
                for id in self.watched.keys().copied().collect::<Vec<_>>() {
                    self.rescan_full.insert(id);
                }
            }
            for p in &ev.paths {
                for (id, p) in self.owners(p) {
                    let pend = self.pending.entry(id).or_default();
                    pend.rescans.insert(p);
                    pend.touch(now);
                }
            }
            return;
        }
        match ev.kind {
            EventKind::Access(_) => {}
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                self.flush_rename();
                self.rename_from = ev.paths.into_iter().next();
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                let to = ev.paths.into_iter().next();
                match (self.rename_from.take(), to) {
                    (Some(from), Some(to)) => self.rename(from, to, now),
                    (None, Some(to)) => self.path(&to, now),
                    (Some(from), None) => self.path(&from, now),
                    (None, None) => {}
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                self.flush_rename();
                let mut it = ev.paths.into_iter();
                let (from, to) = (it.next().expect("two"), it.next().expect("two"));
                self.rename(from, to, now);
            }
            _ => {
                self.flush_rename();
                for p in &ev.paths {
                    self.path(p, now);
                }
            }
        }
    }

    fn flush_rename(&mut self) {
        if let Some(from) = self.rename_from.take() {
            self.path(&from, Instant::now());
        }
    }

    fn path(&mut self, p: &Path, now: Instant) {
        for (id, p) in self.owners(p) {
            let pend = self.pending.entry(id).or_default();
            pend.paths.insert(p);
            pend.touch(now);
        }
    }

    fn rename(&mut self, from: PathBuf, to: PathBuf, now: Instant) {
        let from_owners = self.owners(&from);
        let to_owners = self.owners(&to);
        for (id, f) in &from_owners {
            match to_owners.iter().find(|(tid, _)| tid == id) {
                Some((_, t)) => {
                    let pend = self.pending.entry(*id).or_default();
                    pend.renames.push((f.clone(), t.clone()));
                    pend.touch(now);
                }
                None => self.path(f, now),
            }
        }
        for (id, t) in &to_owners {
            if !from_owners.iter().any(|(fid, _)| fid == id) {
                let pend = self.pending.entry(*id).or_default();
                pend.paths.insert(t.clone());
                pend.touch(now);
            }
        }
    }

    // ----- applying ------------------------------------------------------------------

    fn apply_due(&mut self, now: Instant) {
        let due: Vec<VolumeId> = self
            .pending
            .iter()
            .filter(|(_, p)| p.due(now))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let Some(mut pend) = self.pending.remove(&id) else {
                continue;
            };
            if pend.is_empty() {
                continue;
            }
            if cfg!(windows) && pend.flooded() {
                // The buffer may have overflowed (Windows drops the batch, or the whole
                // watch, without saying so): watch again, and compare the area of the
                // burst with the disk.
                self.rearm(id);
                match self.burst_area(id, &pend) {
                    Some(area) => {
                        log::info!(
                            "live: {} changes at once; checking {} again",
                            pend.events,
                            area.display()
                        );
                        pend.paths.retain(|p| !p.starts_with(&area));
                        pend.rescans.insert(area);
                    }
                    None => {
                        log::info!(
                            "live: {} changes at once; rescanning the drive",
                            pend.events
                        );
                        self.rescan_full.insert(id);
                        continue;
                    }
                }
            }
            let changes = to_changes(&pend);
            match self.catalog.apply_changes(id, &changes) {
                Ok(stats) => {
                    if stats.changed() {
                        log::debug!("live: volume {id}: {stats:?}");
                        let _ = self.app.emit("catalog-changed", id);
                        let _ = self.app.emit("volumes-changed", ());
                    }
                    if stats.to_enrich() > 0 {
                        self.enrich_wanted.insert(id);
                    }
                }
                Err(e) if e.is_busy() => {
                    // A scan or enrichment holds the catalogue: try again shortly.
                    self.requeue(id, pend, now, false);
                }
                Err(Error::VolumeOffline) | Err(Error::VolumeNotFound(_)) => {}
                Err(e) => {
                    log::warn!("live: could not apply changes to volume {id}: {e}");
                    if pend.failures + 1 >= MAX_FAILURES {
                        self.rescan_full.insert(id);
                    } else {
                        self.requeue(id, pend, now, true);
                    }
                }
            }
        }
    }

    /// Put changes back in front of anything that arrived since.
    fn requeue(&mut self, id: VolumeId, mut old: Pending, now: Instant, failed: bool) {
        // Only events that arrive from now on count towards a flood: these were judged.
        old.events = 0;
        if let Some(newer) = self.pending.remove(&id) {
            old.renames.extend(newer.renames);
            old.rescans.extend(newer.rescans);
            old.paths.extend(newer.paths);
            old.events = newer.events;
        }
        if failed {
            old.failures += 1;
        }
        old.first = Some(now);
        old.last = Some(now);
        self.pending.insert(id, old);
    }

    /// The deepest folder holding every path of a burst, unless that's the drive itself.
    fn burst_area(&self, id: VolumeId, pend: &Pending) -> Option<PathBuf> {
        let root = &self.watched.get(&id)?.root;
        let mut area: Option<PathBuf> = None;
        let all = pend
            .paths
            .iter()
            .chain(pend.rescans.iter())
            .chain(pend.renames.iter().flat_map(|(a, b)| [a, b]));
        for p in all {
            let dir = p.parent().unwrap_or(p).to_path_buf();
            area = Some(match area {
                None => dir,
                Some(a) => common_ancestor(&a, &dir),
            });
        }
        let area = area?;
        let rel = self.catalog.live_rel_path(root, &area)?;
        (!rel.is_empty()).then_some(area)
    }

    fn start_rescans(&mut self) {
        let now = storecat_core::now_unix();
        for id in self.rescan_full.iter().copied().collect::<Vec<_>>() {
            if !self.catalog.is_scanning(id) {
                let last = self
                    .catalog
                    .volume(id)
                    .ok()
                    .and_then(|v| v.last_scan_at)
                    .unwrap_or(0);
                if now - last < FULL_RESCAN_GAP_SECS {
                    continue; // asked again on the next pass
                }
            }
            self.rescan_full.remove(&id);
            self.pending.remove(&id);
            if self.catalog.is_scanning(id) {
                continue;
            }
            log::info!("live: rescanning volume {id} in the background");
            crate::commands::spawn_scan(self.app.clone(), Arc::clone(&self.catalog), id);
        }
    }

    fn start_enrichment(&mut self, now: Instant) {
        if self.enrich_wanted.is_empty() {
            return;
        }
        if !self.catalog.setting_enabled(settings::AUTO_ENRICH, true) {
            self.enrich_wanted.clear();
            return;
        }
        let ready: Vec<VolumeId> = self
            .enrich_wanted
            .iter()
            .copied()
            .filter(|id| {
                !self.catalog.is_enriching(*id)
                    && !self.catalog.is_scanning(*id)
                    && self
                        .last_enrich
                        .get(id)
                        .map(|t| now.duration_since(*t) >= ENRICH_EVERY)
                        .unwrap_or(true)
            })
            .collect();
        for id in ready {
            self.enrich_wanted.remove(&id);
            self.last_enrich.insert(id, now);
            crate::commands::spawn_enrich(self.app.clone(), Arc::clone(&self.catalog), id);
        }
    }
}

/// Renames first, while the catalogue still has the old names; then folders to compare
/// again; then single paths, which are checked against the disk as it is by then.
fn to_changes(p: &Pending) -> Vec<Change> {
    let mut out: Vec<Change> = p
        .renames
        .iter()
        .map(|(from, to)| Change::Rename {
            from: from.clone(),
            to: to.clone(),
        })
        .collect();
    out.extend(p.rescans.iter().cloned().map(Change::Rescan));
    out.extend(
        p.paths
            .iter()
            .filter(|path| !p.rescans.iter().any(|r| path.starts_with(r)))
            .cloned()
            .map(Change::Path),
    );
    out
}

fn common_ancestor(a: &Path, b: &Path) -> PathBuf {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect()
}

/// `\\?\C:\x` -> `C:\x`, as volumes are catalogued.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fast_floods_count_as_overflow() {
        let start = Instant::now();
        let batch = |events: usize, secs: u64| Pending {
            events,
            first: Some(start),
            last: Some(start + Duration::from_secs(secs)),
            ..Default::default()
        };
        // A busy system drive: thousands of changes over a few seconds.
        assert!(!batch(4_700, 10).flooded());
        assert!(!batch(4_999, 1).flooded());
        // Tens of thousands at once.
        assert!(batch(20_000, 2).flooded());
        assert!(batch(6_000, 0).flooded());
        assert!(!Pending::default().flooded());
    }

    #[test]
    fn changes_put_renames_first_and_fold_paths_into_rescans() {
        let mut p = Pending::default();
        p.paths.insert(PathBuf::from("/v/a/x.txt"));
        p.paths.insert(PathBuf::from("/v/b.txt"));
        p.rescans.insert(PathBuf::from("/v/a"));
        p.renames
            .push((PathBuf::from("/v/c.txt"), PathBuf::from("/v/d.txt")));
        let c = to_changes(&p);
        assert_eq!(
            c,
            vec![
                Change::Rename {
                    from: PathBuf::from("/v/c.txt"),
                    to: PathBuf::from("/v/d.txt")
                },
                Change::Rescan(PathBuf::from("/v/a")),
                Change::Path(PathBuf::from("/v/b.txt")),
            ]
        );
    }

    #[test]
    fn batches_wait_for_quiet_but_not_forever() {
        let t0 = Instant::now();
        let mut p = Pending::default();
        p.touch(t0);
        assert!(!p.due(t0 + Duration::from_millis(500)));
        assert!(p.due(t0 + QUIET));
        p.touch(t0 + Duration::from_secs(9));
        assert!(!p.due(t0 + Duration::from_millis(9500)));
        assert!(p.due(t0 + MAX_WAIT));
    }

    #[test]
    fn common_ancestor_of_two_folders() {
        assert_eq!(
            common_ancestor(Path::new("/v/a/b/c"), Path::new("/v/a/d")),
            PathBuf::from("/v/a")
        );
    }
}
