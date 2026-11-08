//! Operations: journaled copy / move / delete jobs.
//!
//! A job is a list of items in `master.db`. Each item's status is written as it changes,
//! so a crash, an unplugged drive or a pause loses at most the file in flight, and the
//! job resumes from where it stopped. Copies stream through a BLAKE3 hasher into a
//! temporary file that is renamed into place; verification re-reads the destination.
//! Moves delete the source only after the destination verified. Deletes go to the
//! recycle bin, never straight to `unlink`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::catalog::Catalog;
use crate::error::{Error, Result};
use crate::model::*;

const BUF: usize = 1024 * 1024;

/// Delete check: the file must be byte-for-byte identical to the file at the path that
/// follows (the copy being kept), which must still exist.
pub const CHECK_SAME_AS: &str = "check:same-as:";
/// Delete check: the file must be a macOS AppleDouble companion (`._name`).
pub const CHECK_APPLEDOUBLE: &str = "check:appledouble";
const TEMP_SUFFIX: &str = ".storecat-part";

/// Expand a spec's sources into (source, dest, size, mtime) items.
fn expand(
    spec: &JobSpec,
    plan: &mut JobPlan,
    collect: bool,
) -> Result<Vec<(String, String, u64, i64)>> {
    let mut items = Vec::new();
    let dest_root = PathBuf::from(&spec.dest_root);
    for (i, src) in spec.sources.iter().enumerate() {
        let p = Path::new(src);
        let Ok(md) = fs::metadata(p) else {
            plan.missing_sources.push(src.clone());
            continue;
        };
        if md.is_dir() {
            plan.folders += 1;
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "folder".into());
            let base = dest_root.join(&name);
            walk_into(p, &base, spec.kind, plan, collect, &mut items)?;
        } else {
            // Delete items carry their check where copies carry a destination.
            let dest = match spec.kind {
                JobKind::Delete => spec.checks.get(i).cloned().unwrap_or_default(),
                _ => dest_root
                    .join(p.file_name().unwrap_or_default())
                    .to_string_lossy()
                    .into_owned(),
            };
            push_item(p, &md, dest, spec.kind, plan, collect, &mut items);
        }
    }
    Ok(items)
}

fn walk_into(
    dir: &Path,
    dest_dir: &Path,
    kind: JobKind,
    plan: &mut JobPlan,
    collect: bool,
    items: &mut Vec<(String, String, u64, i64)>,
) -> Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => {
            plan.missing_sources
                .push(dir.to_string_lossy().into_owned());
            return Ok(());
        }
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            plan.folders += 1;
            walk_into(
                &path,
                &dest_dir.join(entry.file_name()),
                kind,
                plan,
                collect,
                items,
            )?;
        } else if let Ok(md) = entry.metadata() {
            let dest = match kind {
                JobKind::Delete => String::new(),
                _ => dest_dir
                    .join(entry.file_name())
                    .to_string_lossy()
                    .into_owned(),
            };
            push_item(&path, &md, dest, kind, plan, collect, items);
        }
    }
    Ok(())
}

fn push_item(
    src: &Path,
    md: &fs::Metadata,
    dest: String,
    kind: JobKind,
    plan: &mut JobPlan,
    collect: bool,
    items: &mut Vec<(String, String, u64, i64)>,
) {
    let size = md.len();
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    plan.files += 1;
    plan.bytes += size;
    if kind != JobKind::Delete && Path::new(&dest).exists() {
        plan.conflicts += 1;
    }
    let src_s = src.to_string_lossy().into_owned();
    if plan.sample.len() < 12 {
        plan.sample.push((src_s.clone(), dest.clone()));
    }
    if collect {
        items.push((src_s, dest, size, mtime));
    }
}

/// Free space on the filesystem holding `path` (or its nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    loop {
        if p.exists() {
            return fs4::available_space(p).ok();
        }
        p = p.parent()?;
    }
}

/// Copy `src` to `dst` through a hasher, atomically (temp file + rename), preserving
/// mtime. Returns the BLAKE3 of what was written.
fn copy_hashed(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> std::io::Result<[u8; 32]> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = PathBuf::from(format!("{}{}", dst.to_string_lossy(), TEMP_SUFFIX));
    let mut input = File::open(src)?;
    let mut out = File::create(&tmp)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BUF];
    let result = (|| -> std::io::Result<()> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("cancelled"));
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            on_bytes(n as u64);
        }
        out.flush()?;
        if let Ok(md) = input.metadata() {
            if let Ok(m) = md.modified() {
                let _ = out.set_modified(m);
            }
        }
        Ok(())
    })();
    drop(out);
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if dst.exists() {
        fs::remove_file(dst)?;
    }
    fs::rename(&tmp, dst)?;
    Ok(*hasher.finalize().as_bytes())
}

fn hash_file(path: &Path, cancel: &AtomicBool) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; BUF];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(*h.finalize().as_bytes())
}

/// The keeper's hash, remembered between the items of one duplicate group.
static KEEPER_HASH: std::sync::Mutex<Option<(PathBuf, u64, [u8; 32])>> =
    std::sync::Mutex::new(None);

/// Refuse to delete a file that no longer meets the condition it was planned under.
fn delete_check(src: &Path, check: &str, cancel: &AtomicBool) -> Result<()> {
    let refuse = |why: &str| Err(Error::Other(format!("left in place: {why}")));
    if check.is_empty() {
        return Ok(());
    }
    if check == CHECK_APPLEDOUBLE {
        let mut head = [0u8; 4];
        let ok = File::open(src)
            .and_then(|mut f| f.read_exact(&mut head))
            .is_ok();
        return if ok && head == crate::places::APPLEDOUBLE_MAGIC {
            Ok(())
        } else {
            refuse("not a macOS metadata file")
        };
    }
    let Some(keep) = check.strip_prefix(CHECK_SAME_AS) else {
        return refuse("unknown check");
    };
    let keep = Path::new(keep);
    if keep == src {
        return refuse("it is the copy being kept");
    }
    let (Ok(km), Ok(sm)) = (fs::metadata(keep), fs::metadata(src)) else {
        return refuse("the copy being kept is missing");
    };
    if km.len() != sm.len() {
        return refuse("it differs from the copy being kept");
    }
    let cancelled = |e: std::io::Error| Error::Other(e.to_string());
    let keep_hash = {
        let cached = KEEPER_HASH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|(p, len, _)| p == keep && *len == km.len())
            .map(|(_, _, h)| h);
        match cached {
            Some(h) => h,
            None => {
                let h = hash_file(keep, cancel).map_err(cancelled)?;
                *KEEPER_HASH.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((keep.to_path_buf(), km.len(), h));
                h
            }
        }
    };
    if hash_file(src, cancel).map_err(cancelled)? != keep_hash {
        return refuse("it differs from the copy being kept");
    }
    Ok(())
}

/// `photo.jpg` → `photo (2).jpg`, `photo (3).jpg`, …
fn renamed(dest: &Path) -> PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let parent = dest.parent().unwrap_or(Path::new(""));
    for n in 2..10_000 {
        let candidate = parent.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} ({}){ext}", crate::model::now_unix()))
}

fn same_stamp(src: &Path, dst: &Path, size: u64, mtime: i64) -> bool {
    let Ok(md) = fs::metadata(dst) else {
        return false;
    };
    if md.len() != size {
        return false;
    }
    let dst_m = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(-1);
    let _ = src;
    (dst_m - mtime).abs() <= 2
}

impl Catalog {
    /// Which catalogued, connected volume a path lives on (longest mount-point match).
    pub fn volume_for_path(&self, path: &Path) -> Option<VolumeId> {
        let p = path.to_string_lossy().to_lowercase();
        let mut best: Option<(usize, VolumeId)> = None;
        for v in self.volumes().ok()?.iter().filter(|v| v.online) {
            let Some(m) = &v.mount_path else { continue };
            let m = m.to_lowercase();
            let m = m.trim_end_matches(['\\', '/']);
            let matches =
                p == m || p.starts_with(&format!("{m}\\")) || p.starts_with(&format!("{m}/"));
            if matches && best.map(|(len, _)| m.len() > len).unwrap_or(true) {
                best = Some((m.len(), v.id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Dry run: what a job would do, and whether it fits.
    pub fn plan_job(&self, spec: &JobSpec) -> Result<JobPlan> {
        let mut plan = JobPlan::default();
        expand(spec, &mut plan, false)?;
        if spec.kind != JobKind::Delete {
            let dest = Path::new(&spec.dest_root);
            plan.dest_free = free_space(dest);
            plan.enough_space = plan
                .dest_free
                .map(|f| f > plan.bytes + 64 * 1024 * 1024)
                .unwrap_or(true);
            plan.dest_volume_id = self.volume_for_path(dest);
        } else {
            plan.enough_space = true;
        }
        plan.source_volume_id = spec
            .sources
            .first()
            .and_then(|s| self.volume_for_path(Path::new(s)));
        Ok(plan)
    }

    /// Journal a job. Nothing is copied until `run_job`.
    pub fn create_job(&self, spec: &JobSpec) -> Result<Job> {
        if spec.sources.is_empty() {
            return Err(Error::Other("nothing to do: no sources".into()));
        }
        if spec.kind != JobKind::Delete && spec.dest_root.trim().is_empty() {
            return Err(Error::Other("a destination folder is required".into()));
        }
        if spec.kind != JobKind::Delete {
            let dest = Path::new(&spec.dest_root);
            for s in &spec.sources {
                let sp = Path::new(s);
                if dest.starts_with(sp) {
                    return Err(Error::Other(format!(
                        "destination {} is inside source {}",
                        dest.display(),
                        sp.display()
                    )));
                }
            }
        }
        let mut plan = JobPlan::default();
        let items = expand(spec, &mut plan, true)?;
        if items.is_empty() {
            return Err(Error::Other(
                "nothing to do: sources are empty or unreadable".into(),
            ));
        }
        let source_volume = spec
            .sources
            .first()
            .and_then(|s| self.volume_for_path(Path::new(s)));
        let dest_volume = if spec.kind == JobKind::Delete {
            None
        } else {
            self.volume_for_path(Path::new(&spec.dest_root))
        };
        let master = self.master();
        let id = master.insert_job(
            spec.kind,
            &spec.name,
            source_volume,
            dest_volume,
            &spec.dest_root,
            spec.conflict,
            spec.verify,
        )?;
        master.insert_job_items(id, &items)?;
        master
            .job(id)?
            .ok_or_else(|| Error::Other("job vanished".into()))
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        self.master().jobs()
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.master()
            .job(id)?
            .ok_or_else(|| Error::Other(format!("job {id} not found")))
    }

    pub fn job_items(
        &self,
        id: i64,
        status: Option<ItemStatus>,
        limit: usize,
    ) -> Result<Vec<JobItem>> {
        self.master().job_items(id, status, limit)
    }

    pub fn delete_job(&self, id: i64) -> Result<()> {
        if self.is_job_running(id) {
            return Err(Error::Other("job is running".into()));
        }
        self.master().delete_job(id)
    }

    pub fn is_job_running(&self, id: i64) -> bool {
        self.running_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id)
    }

    pub fn running_job_ids(&self) -> Vec<i64> {
        self.running_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    /// Ask a running job to stop after the current file. It can be resumed with `run_job`.
    pub fn pause_job(&self, id: i64) {
        if let Some(flag) = self
            .running_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Mark a job cancelled; pending items stay pending for the record.
    pub fn cancel_job(&self, id: i64) -> Result<()> {
        self.pause_job(id);
        if !self.is_job_running(id) {
            self.master()
                .set_job_status(id, JobStatus::Cancelled, None)?;
        } else {
            let mut set = self
                .cancelled_jobs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            set.insert(id);
        }
        Ok(())
    }

    /// Run (or resume) a job to completion. Blocking: run on a worker thread.
    pub fn run_job(&self, id: i64, progress: &(dyn Fn(JobProgress) + Sync)) -> Result<Job> {
        let job = self.job(id)?;
        if matches!(job.status, JobStatus::Done | JobStatus::Cancelled) {
            return Ok(job);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut running = self.running_jobs.lock().unwrap_or_else(|e| e.into_inner());
            if running.contains_key(&id) {
                return Err(Error::Other("job is already running".into()));
            }
            running.insert(id, Arc::clone(&cancel));
        }
        self.master().set_job_status(id, JobStatus::Running, None)?;
        let result = self.run_job_inner(&job, &cancel, progress);
        self.running_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let was_cancelled = self
            .cancelled_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let master = self.master();
        master.refresh_job_counts(id)?;
        match &result {
            Ok(finished) if was_cancelled => {
                master.set_job_status(id, JobStatus::Cancelled, None)?
            }
            Ok(finished) if !*finished => master.set_job_status(id, JobStatus::Paused, None)?,
            Ok(_) => {
                let j = master.job(id)?.unwrap();
                let status = if j.files_failed > 0 {
                    JobStatus::Failed
                } else {
                    JobStatus::Done
                };
                let err = if j.files_failed > 0 {
                    Some(format!(
                        "{} file(s) failed; see the job's items",
                        j.files_failed
                    ))
                } else {
                    None
                };
                master.set_job_status(id, status, err.as_deref())?;
            }
            Err(e) => master.set_job_status(id, JobStatus::Failed, Some(&e.to_string()))?,
        }
        drop(master);
        let final_job = self.job(id)?;
        progress(JobProgress {
            job_id: id,
            status: final_job.status,
            files_done: final_job.files_done,
            files_total: final_job.files_total,
            bytes_done: final_job.bytes_done,
            bytes_total: final_job.bytes_total,
            files_failed: final_job.files_failed,
            current: String::new(),
            bytes_per_sec: 0,
            elapsed_ms: 0,
        });
        result.map(|_| final_job)
    }

    /// Returns Ok(true) when every item reached a terminal state, Ok(false) when paused.
    fn run_job_inner(
        &self,
        job: &Job,
        cancel: &AtomicBool,
        progress: &(dyn Fn(JobProgress) + Sync),
    ) -> Result<bool> {
        let started = Instant::now();
        let mut bytes_session = 0u64;
        let mut last_report = Instant::now();
        let mut done_files = job.files_done;
        let mut done_bytes = job.bytes_done;
        let mut failed = job.files_failed;
        let mut source_dirs: HashSet<PathBuf> = HashSet::new();

        let report =
            |current: &str, done_files: u64, done_bytes: u64, failed: u64, bytes_session: u64| {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                progress(JobProgress {
                    job_id: job.id,
                    status: JobStatus::Running,
                    files_done: done_files,
                    files_total: job.files_total,
                    bytes_done: done_bytes,
                    bytes_total: job.bytes_total,
                    files_failed: failed,
                    current: current.to_string(),
                    bytes_per_sec: (bytes_session as f64 / secs) as u64,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            };

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            // Pending first, then anything copied-but-unfinished from an earlier run.
            let mut batch = self
                .master()
                .job_items(job.id, Some(ItemStatus::Pending), 200)?;
            if batch.is_empty() {
                batch = self
                    .master()
                    .job_items(job.id, Some(ItemStatus::Copied), 200)?;
            }
            if batch.is_empty() {
                batch = self
                    .master()
                    .job_items(job.id, Some(ItemStatus::Verified), 200)?;
            }
            if batch.is_empty() {
                break;
            }
            for item in batch {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let src = PathBuf::from(&item.source);
                if let Some(parent) = src.parent() {
                    source_dirs.insert(parent.to_path_buf());
                }
                let name = src
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let outcome = self.run_item(job, &item, cancel, &mut |n| {
                    bytes_session += n;
                    if last_report.elapsed().as_millis() >= 250 {
                        last_report = Instant::now();
                        report(&name, done_files, done_bytes, failed, bytes_session);
                    }
                });
                match outcome {
                    Ok(ItemStatus::Skipped) => {
                        self.master()
                            .set_item_status(item.id, ItemStatus::Skipped, None, None)?;
                    }
                    Ok(status) => {
                        done_files += 1;
                        done_bytes += item.size;
                        let _ = status;
                    }
                    Err(e) if e.to_string().contains("cancelled") => return Ok(false),
                    Err(e) => {
                        failed += 1;
                        self.master().set_item_status(
                            item.id,
                            ItemStatus::Failed,
                            None,
                            Some(&e.to_string()),
                        )?;
                    }
                }
                if last_report.elapsed().as_millis() >= 250 {
                    last_report = Instant::now();
                    report(&name, done_files, done_bytes, failed, bytes_session);
                }
            }
            self.master().refresh_job_counts(job.id)?;
        }

        // Moves: tidy source folders that are now empty (never the roots the user chose).
        if job.kind == JobKind::Move {
            let mut dirs: Vec<PathBuf> = source_dirs.into_iter().collect();
            dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
            for d in dirs {
                if fs::read_dir(&d)
                    .map(|mut r| r.next().is_none())
                    .unwrap_or(false)
                {
                    let is_root = self.job_roots(job.id).iter().any(|r| Path::new(r) == d);
                    if !is_root {
                        let _ = fs::remove_dir(&d);
                    }
                }
            }
        }
        report("", done_files, done_bytes, failed, bytes_session);
        Ok(true)
    }

    /// The top-level folders a job was created from, inferred from its items.
    fn job_roots(&self, id: i64) -> Vec<String> {
        let items = self
            .master()
            .job_items(id, None, 100_000)
            .unwrap_or_default();
        let mut roots: HashMap<String, usize> = HashMap::new();
        for it in items {
            let src = Path::new(&it.source);
            let dst = Path::new(&it.dest);
            // Walk up as many components as the destination shares with dest_root.
            let depth = dst
                .strip_prefix(self.job(id).map(|j| j.dest_root).unwrap_or_default())
                .map(|rel| rel.components().count())
                .unwrap_or(1);
            // `depth` counts the file itself; the chosen root is one level above the rest.
            let mut root = src.to_path_buf();
            for _ in 0..depth.saturating_sub(1).max(1) {
                if let Some(p) = root.parent() {
                    root = p.to_path_buf();
                }
            }
            *roots
                .entry(root.to_string_lossy().into_owned())
                .or_default() += 1;
        }
        roots.into_keys().collect()
    }

    fn run_item(
        &self,
        job: &Job,
        item: &JobItem,
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<ItemStatus> {
        let src = Path::new(&item.source);
        let master = || self.master();

        if job.kind == JobKind::Delete {
            if !src.exists() {
                master().set_item_status(item.id, ItemStatus::Done, None, None)?;
                return Ok(ItemStatus::Done);
            }
            delete_check(src, &item.dest, cancel)?;
            trash::delete(src).map_err(|e| Error::Other(format!("recycle bin: {e}")))?;
            master().set_item_status(item.id, ItemStatus::Done, None, None)?;
            return Ok(ItemStatus::Done);
        }

        let mut dest = PathBuf::from(&item.dest);
        let mut status = item.status;

        if status == ItemStatus::Pending {
            if !src.exists() {
                return Err(Error::Other("source file no longer exists".into()));
            }
            if dest.exists() {
                if same_stamp(src, &dest, item.size, item.mtime) {
                    // Looks like the same file already there: prove it, then treat as copied.
                    let a = hash_file(src, cancel).map_err(|e| Error::Other(e.to_string()))?;
                    let b = hash_file(&dest, cancel).map_err(|e| Error::Other(e.to_string()))?;
                    if a == b {
                        master().set_item_status(item.id, ItemStatus::Verified, Some(&a), None)?;
                        status = ItemStatus::Verified;
                    }
                }
                if status == ItemStatus::Pending {
                    match job.conflict {
                        ConflictPolicy::Skip => return Ok(ItemStatus::Skipped),
                        ConflictPolicy::Rename => {
                            dest = renamed(&dest);
                            self.master_update_item_dest(item.id, &dest)?;
                        }
                        ConflictPolicy::Overwrite => {}
                    }
                }
            }
            if status == ItemStatus::Pending {
                let hash = copy_hashed(src, &dest, cancel, on_bytes)
                    .map_err(|e| Error::Other(format!("copy: {e}")))?;
                master().set_item_status(item.id, ItemStatus::Copied, Some(&hash), None)?;
                status = ItemStatus::Copied;
            }
        }

        if status == ItemStatus::Copied {
            if job.verify {
                let expected = self.master_item_hash(item.id)?;
                let actual =
                    hash_file(&dest, cancel).map_err(|e| Error::Other(format!("verify: {e}")))?;
                if expected.as_deref() != Some(&actual[..]) {
                    let _ = fs::remove_file(&dest);
                    master().set_item_status(item.id, ItemStatus::Pending, None, None)?;
                    return Err(Error::Other(
                        "verification failed: destination differs from source".into(),
                    ));
                }
            }
            master().set_item_status(item.id, ItemStatus::Verified, None, None)?;
            status = ItemStatus::Verified;
        }

        if status == ItemStatus::Verified {
            if job.kind == JobKind::Move && src.exists() {
                fs::remove_file(src).map_err(|e| Error::Other(format!("delete source: {e}")))?;
            }
            master().set_item_status(item.id, ItemStatus::Done, None, None)?;
            status = ItemStatus::Done;
        }
        Ok(status)
    }

    fn master_update_item_dest(&self, item_id: i64, dest: &Path) -> Result<()> {
        self.master()
            .set_item_dest(item_id, &dest.to_string_lossy())
    }

    fn master_item_hash(&self, item_id: i64) -> Result<Option<Vec<u8>>> {
        self.master().item_hash(item_id)
    }

    // ----- presets and helpers ---------------------------------------------------------

    /// The quarterly sweep: Desktop and Downloads into `<dest_drive>\\Archive\\<YYYY-MM> Desktop+Downloads`.
    pub fn sweep_spec(&self, dest_mount: &str, kind: JobKind) -> JobSpec {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let mut sources = Vec::new();
        for name in ["Desktop", "Downloads"] {
            let p = home.join(name);
            if p.is_dir() {
                sources.push(p.to_string_lossy().into_owned());
            }
        }
        let now = time::OffsetDateTime::now_utc();
        let stamp = format!("{:04}-{:02}", now.year(), now.month() as u8);
        let dest = Path::new(dest_mount)
            .join("Archive")
            .join(format!("{stamp} Desktop+Downloads"));
        JobSpec {
            kind,
            name: format!("Desktop + Downloads sweep {stamp}"),
            sources,
            dest_root: dest.to_string_lossy().into_owned(),
            conflict: ConflictPolicy::Rename,
            verify: true,
            checks: Vec::new(),
        }
    }

    /// A delete job that keeps one copy in each duplicate group and recycles the other
    /// copies of yours. Copies in app-managed folders and on disconnected drives are left
    /// alone. Each deletion re-checks, byte for byte, that the file still matches the copy
    /// being kept, so groups found by quick hash need no separate verify step.
    pub fn duplicate_cleanup_spec(&self, picks: &[DupPick]) -> Result<JobSpec> {
        let volumes = self.volumes()?;
        let mount_of = |vid: VolumeId| -> Option<&str> {
            volumes
                .iter()
                .find(|v| v.id == vid && v.online)
                .and_then(|v| v.mount_path.as_deref())
        };
        let mut sources = Vec::new();
        let mut checks = Vec::new();
        let mut groups = 0usize;
        let mut first_name = String::new();
        for pick in picks {
            let hash =
                crate::enrich::unhex(&pick.hash).ok_or_else(|| Error::Other("bad hash".into()))?;
            let Some(group) = self.duplicate_group(&hash, Some(&volumes))? else {
                continue;
            };
            let keep = (pick.keep_volume_id, pick.keep_file_id);
            let Some(keeper) = group
                .members
                .iter()
                .find(|m| (m.volume_id, m.file_id) == keep)
            else {
                return Err(Error::Other(format!(
                    "the copy to keep is not in the group of {}",
                    group.name
                )));
            };
            let Some(keep_mount) = mount_of(keeper.volume_id) else {
                return Err(Error::Other(format!(
                    "connect {} first: the copy of {} to keep is on it",
                    keeper.volume_label, group.name
                )));
            };
            let keep_path = crate::catalog::join_abs(keep_mount, &keeper.path, &keeper.name);
            let before = sources.len();
            for m in &group.members {
                if (m.volume_id, m.file_id) == keep || m.app_managed {
                    continue;
                }
                let Some(mount) = mount_of(m.volume_id) else {
                    continue;
                };
                sources.push(crate::catalog::join_abs(mount, &m.path, &m.name));
                checks.push(format!("{CHECK_SAME_AS}{keep_path}"));
            }
            if sources.len() > before {
                groups += 1;
                if first_name.is_empty() {
                    first_name = group.name.clone();
                }
            }
        }
        if sources.is_empty() {
            return Err(Error::Other(
                "nothing to remove: the other copies are in app folders or on disconnected drives"
                    .into(),
            ));
        }
        let n = sources.len();
        let copies = if n == 1 { "copy" } else { "copies" };
        let name = if groups == 1 {
            format!("Remove {n} extra {copies} of {first_name}")
        } else {
            format!("Remove {n} extra {copies} from {groups} duplicate groups")
        };
        Ok(JobSpec {
            kind: JobKind::Delete,
            name,
            sources,
            dest_root: String::new(),
            conflict: ConflictPolicy::Skip,
            verify: false,
            checks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::DataDirs;
    use crate::scan::ScanOptions;

    fn setup() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<Catalog>,
    ) {
        let data = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("Desktop/project")).unwrap();
        fs::create_dir_all(src.path().join("Downloads")).unwrap();
        fs::write(src.path().join("Desktop/notes.txt"), b"hello desktop").unwrap();
        fs::write(
            src.path().join("Desktop/project/plan.md"),
            vec![7u8; 3_000_000],
        )
        .unwrap();
        fs::write(src.path().join("Downloads/setup.exe"), vec![1u8; 500_000]).unwrap();
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        (data, src, dst, cat)
    }

    /// Duplicate cleanup keeps the best copy of yours, never touches app copies, and each
    /// deletion re-checks the file against the copy being kept.
    #[test]
    fn duplicate_cleanup_keeps_one_and_checks_each_copy() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let t = tree.path();
        let blob: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
        for p in [
            "Photos/beach.bin",
            "Downloads/beach (1).bin",
            "AppData/Local/Temp/wetransfer/beach.bin",
            ".cache/thumbs/beach.bin",
        ] {
            fs::create_dir_all(t.join(p).parent().unwrap()).unwrap();
            fs::write(t.join(p), &blob).unwrap();
        }
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(t, Some("Dupes")).unwrap();
        cat.scan(v.id, &ScanOptions::default(), &|_| {}).unwrap();
        cat.enrich(v.id, &crate::EnrichOptions::default(), &|_| {})
            .unwrap();

        let q = |scope| DupQuery {
            scope,
            sort: DupSort::Waste,
            desc: true,
            limit: 10,
            offset: 0,
            ..Default::default()
        };
        let mine = cat.duplicate_page(&q(DupScope::Mine)).unwrap();
        assert_eq!(mine.groups.len(), 1);
        let g = &mine.groups[0];
        assert_eq!(g.count, 4);
        assert_eq!(g.user_copies, 3);
        assert_eq!(g.waste, 4_000_000, "the app copy doesn't count");
        assert_eq!(mine.summary.files, 3);
        let keep = g.suggested_keep.unwrap();
        let keeper = g
            .members
            .iter()
            .find(|m| (m.volume_id, m.file_id) == keep)
            .unwrap();
        assert_eq!(keeper.path, "Photos");
        assert!(g
            .members
            .iter()
            .any(|m| m.app_managed && m.path == ".cache/thumbs"));
        assert_eq!(
            cat.duplicate_page(&q(DupScope::All)).unwrap().summary.files,
            4
        );
        let filtered = |f: DupQuery| cat.duplicate_page(&f).unwrap().groups.len();
        let mine = || q(DupScope::Mine);
        assert_eq!(
            filtered(DupQuery {
                text: "wetransfer".into(),
                ..mine()
            }),
            1
        );
        assert_eq!(
            filtered(DupQuery {
                text: "invoice".into(),
                ..mine()
            }),
            0
        );
        assert_eq!(
            filtered(DupQuery {
                kinds: vec![Kind::Video],
                ..mine()
            }),
            0
        );
        assert_eq!(
            filtered(DupQuery {
                ready_only: true,
                ..mine()
            }),
            1
        );
        assert_eq!(
            filtered(DupQuery {
                volume_id: Some(v.id),
                ..mine()
            }),
            1
        );

        let spec = cat
            .duplicate_cleanup_spec(&[DupPick {
                hash: g.hash.clone(),
                keep_volume_id: keep.0,
                keep_file_id: keep.1,
            }])
            .unwrap();
        assert_eq!(spec.kind, JobKind::Delete);
        assert_eq!(spec.name, "Remove 2 extra copies of beach.bin");
        assert_eq!(spec.sources.len(), 2);
        assert!(spec.sources.iter().all(|s| !s.contains(".cache")));
        assert!(spec.sources.iter().all(|s| !s.contains("Photos")));
        // The catalogue stores the folder's resolved path (macOS `/private/var`, Windows
        // long names instead of `RUNNER~1`), so compare against that, not the temp path.
        let mount = cat
            .volumes()
            .unwrap()
            .into_iter()
            .find(|x| x.id == v.id)
            .and_then(|x| x.mount_path)
            .unwrap();
        let expected_keep = crate::catalog::join_abs(&mount, "Photos", "beach.bin");
        let keep_path = t.join("Photos/beach.bin");
        for (src, check) in spec.sources.iter().zip(&spec.checks) {
            assert_eq!(check.strip_prefix(CHECK_SAME_AS).unwrap(), expected_keep);
            assert!(delete_check(Path::new(src), check, &AtomicBool::new(false)).is_ok());
        }

        // A copy that changed since it was catalogued is left in place.
        let temp_copy = t.join("AppData/Local/Temp/wetransfer/beach.bin");
        let mut changed = blob.clone();
        changed[1234] ^= 0xff;
        fs::write(&temp_copy, &changed).unwrap();
        let check = format!("{CHECK_SAME_AS}{}", keep_path.display());
        let e = delete_check(&temp_copy, &check, &AtomicBool::new(false)).unwrap_err();
        assert!(e.to_string().contains("left in place"), "{e}");
        // So is the keeper itself, and anything when the keeper has gone.
        assert!(delete_check(&keep_path, &check, &AtomicBool::new(false)).is_err());
        let gone = format!("{CHECK_SAME_AS}{}", t.join("nope.bin").display());
        assert!(delete_check(&temp_copy, &gone, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn appledouble_check_reads_the_magic() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("._song.mp3");
        let fake = dir.path().join("._notes.txt");
        let mut bytes = crate::places::APPLEDOUBLE_MAGIC.to_vec();
        bytes.extend_from_slice(b"mac metadata");
        fs::write(&real, bytes).unwrap();
        fs::write(&fake, b"just text").unwrap();
        let no = AtomicBool::new(false);
        assert!(delete_check(&real, CHECK_APPLEDOUBLE, &no).is_ok());
        assert!(delete_check(&fake, CHECK_APPLEDOUBLE, &no).is_err());
        assert!(delete_check(&fake, "", &no).is_ok(), "no check, no refusal");
    }

    #[test]
    fn copy_then_move_with_verify_and_resume() {
        let (_data, src, dst, cat) = setup();
        let dest_root = dst.path().join("Archive").join("2026-11 sweep");
        let spec = JobSpec {
            kind: JobKind::Copy,
            name: "test copy".into(),
            sources: vec![
                src.path().join("Desktop").to_string_lossy().into_owned(),
                src.path().join("Downloads").to_string_lossy().into_owned(),
            ],
            dest_root: dest_root.to_string_lossy().into_owned(),
            conflict: ConflictPolicy::Rename,
            verify: true,
            checks: Vec::new(),
        };
        let plan = cat.plan_job(&spec).unwrap();
        assert_eq!(plan.files, 3);
        assert_eq!(plan.folders, 3);
        assert_eq!(plan.bytes, 3_500_013);
        assert!(plan.enough_space);

        let job = cat.create_job(&spec).unwrap();
        assert_eq!(job.files_total, 3);
        let done = cat.run_job(job.id, &|_| {}).unwrap();
        assert_eq!(done.status, JobStatus::Done);
        assert_eq!(done.files_done, 3);
        assert!(dest_root.join("Desktop/project/plan.md").exists());
        assert!(dest_root.join("Downloads/setup.exe").exists());
        assert!(
            src.path().join("Desktop/notes.txt").exists(),
            "copy keeps originals"
        );
        let items = cat.job_items(job.id, None, 100).unwrap();
        assert!(items.iter().all(|i| i.status == ItemStatus::Done));

        // Running the same copy again: identical files are recognised and nothing duplicated.
        let job2 = cat.create_job(&spec).unwrap();
        let done2 = cat.run_job(job2.id, &|_| {}).unwrap();
        assert_eq!(done2.status, JobStatus::Done);
        assert!(!dest_root.join("Desktop/notes (2).txt").exists());

        // Move into a fresh destination, pausing after the first file, then resume.
        let dest2 = dst.path().join("Moved");
        let spec_move = JobSpec {
            kind: JobKind::Move,
            name: "test move".into(),
            sources: vec![src.path().join("Desktop").to_string_lossy().into_owned()],
            dest_root: dest2.to_string_lossy().into_owned(),
            conflict: ConflictPolicy::Rename,
            verify: true,
            checks: Vec::new(),
        };
        let job3 = cat.create_job(&spec_move).unwrap();
        let cat2 = Arc::clone(&cat);
        let jid = job3.id;
        let paused = cat
            .run_job(jid, &move |p: JobProgress| {
                if p.files_done >= 1 {
                    cat2.pause_job(jid);
                }
            })
            .unwrap();
        assert!(matches!(paused.status, JobStatus::Paused | JobStatus::Done));
        let resumed = cat.run_job(jid, &|_| {}).unwrap();
        assert_eq!(resumed.status, JobStatus::Done);
        assert_eq!(resumed.files_done, 2);
        assert!(dest2.join("Desktop/project/plan.md").exists());
        assert!(
            !src.path().join("Desktop/notes.txt").exists(),
            "move removes originals"
        );
        assert!(
            !src.path().join("Desktop/project").exists(),
            "emptied subfolder removed"
        );
        assert!(src.path().join("Desktop").exists(), "chosen root kept");
    }

    #[test]
    fn conflicts_and_bad_specs() {
        let (_data, src, dst, cat) = setup();
        fs::create_dir_all(dst.path().join("out")).unwrap();
        fs::write(dst.path().join("out/notes.txt"), b"different").unwrap();
        let spec = JobSpec {
            kind: JobKind::Copy,
            name: "conflict".into(),
            sources: vec![src
                .path()
                .join("Desktop/notes.txt")
                .to_string_lossy()
                .into_owned()],
            dest_root: dst.path().join("out").to_string_lossy().into_owned(),
            conflict: ConflictPolicy::Rename,
            verify: true,
            checks: Vec::new(),
        };
        assert_eq!(cat.plan_job(&spec).unwrap().conflicts, 1);
        let job = cat.create_job(&spec).unwrap();
        cat.run_job(job.id, &|_| {}).unwrap();
        assert!(dst.path().join("out/notes (2).txt").exists());
        assert_eq!(
            fs::read(dst.path().join("out/notes.txt")).unwrap(),
            b"different"
        );

        let skip = JobSpec {
            conflict: ConflictPolicy::Skip,
            name: "skip".into(),
            ..spec.clone()
        };
        let job = cat.create_job(&skip).unwrap();
        let j = cat.run_job(job.id, &|_| {}).unwrap();
        assert_eq!(j.files_skipped, 1);

        let inside = JobSpec {
            sources: vec![src.path().to_string_lossy().into_owned()],
            dest_root: src.path().join("Desktop").to_string_lossy().into_owned(),
            ..spec.clone()
        };
        assert!(
            cat.create_job(&inside).is_err(),
            "destination inside source is refused"
        );
        let empty = JobSpec {
            sources: vec![],
            ..spec.clone()
        };
        assert!(cat.create_job(&empty).is_err());
    }

    #[test]
    fn tags_survive_rescans_and_are_searchable() {
        let (_data, src, _dst, cat) = setup();
        let v = cat.add_folder(src.path(), Some("Home")).unwrap();
        let opts = ScanOptions {
            threads: 2,
            ..Default::default()
        };
        cat.scan(v.id, &opts, &|_| {}).unwrap();
        let hit = cat
            .search("notes", &SearchOptions::default())
            .unwrap()
            .hits
            .remove(0);
        let tags = cat
            .tag_item(hit.volume_id, hit.item_id, hit.item_type, "client-x")
            .unwrap();
        assert_eq!(tags, vec!["client-x"]);
        cat.tag_item(hit.volume_id, hit.item_id, hit.item_type, "Urgent")
            .unwrap();
        let r = cat
            .search("tag:client-x", &SearchOptions::default())
            .unwrap();
        assert_eq!(r.hits[0].name, "notes.txt");
        let r = cat.search("urgent", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 1, "tag words are plain-text searchable");
        assert_eq!(cat.tags().unwrap().len(), 2);

        // Rescan reassigns ids; the tag is keyed by path and comes back.
        fs::write(src.path().join("Downloads/extra.bin"), b"x").unwrap();
        cat.scan(v.id, &opts, &|_| {}).unwrap();
        let r = cat
            .search("tag:client-x", &SearchOptions::default())
            .unwrap();
        assert_eq!(r.total, 1, "tag survives rescan");
        let d = cat
            .item_detail(r.hits[0].volume_id, r.hits[0].item_id, r.hits[0].item_type)
            .unwrap();
        assert_eq!(d.tags, vec!["client-x", "Urgent"]);
        cat.untag_item(d.volume_id, d.item_id, d.item_type, "urgent")
            .unwrap();
        assert_eq!(
            cat.search("urgent", &SearchOptions::default())
                .unwrap()
                .total,
            0
        );

        let s = cat.save_search("Client X", "tag:client-x ext:txt").unwrap();
        assert_eq!(cat.saved_searches().unwrap().len(), 1);
        cat.delete_saved_search(s.id).unwrap();
        assert!(cat.saved_searches().unwrap().is_empty());
    }
}
