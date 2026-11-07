//! Rules engine: looks at the catalogue and proposes things worth doing. Every suggestion
//! is either a job spec (shown as a plan you approve in Operations) or a pointer to a
//! view. Nothing here ever moves a file on its own.
//!
//! Rules, in order of urgency:
//! * `health`     a drive reports a S.M.A.R.T. warning: back it up to a Backup drive now.
//! * `full`       a drive is over the fullness threshold: open its treemap.
//! * `sweep`      Desktop + Downloads have grown since the last sweep: archive them.
//! * `cold`       big folders on fast storage untouched for a year: move to an Archive drive.
//! * `duplicates` reclaimable duplicate copies above a threshold.
//! * `overlap`    a folder catalogued twice (inside another catalogued folder or drive).
//! * `stale`      a drive whose catalogue is months old: plug it in to refresh.
//! * `roles`      drives without a role, which the rules above need to choose destinations.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::Result;
use crate::model::*;

/// User-tunable thresholds, stored as JSON in the `rules` setting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleSettings {
    pub enabled: bool,
    /// A folder counts as cold when nothing in it changed for this many days.
    pub cold_days: u32,
    /// ...and it holds at least this many GB.
    pub cold_min_gb: f64,
    /// Suggest a Desktop + Downloads sweep this many days after the last one.
    pub sweep_days: u32,
    /// ...when they hold at least this many GB.
    pub sweep_min_gb: f64,
    /// Warn when a drive is fuller than this percentage.
    pub full_percent: u32,
    /// Mention duplicates once they waste at least this many GB.
    pub duplicates_min_gb: f64,
    /// A catalogue older than this many days is called stale.
    pub stale_days: u32,
}

impl Default for RuleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            cold_days: 365,
            cold_min_gb: 1.0,
            sweep_days: 90,
            sweep_min_gb: 1.0,
            full_percent: 90,
            duplicates_min_gb: 1.0,
            stale_days: 180,
        }
    }
}

const RULES_KEY: &str = "rules";
const GB: f64 = 1024.0 * 1024.0 * 1024.0;
const DAY: i64 = 86_400;
/// Suggest removing OS leftovers from a drive once it has at least this many.
const LEFTOVERS_MIN: u64 = 10;
/// Leftover files per drive (folder, name, size), with the scan they were read after.
#[allow(clippy::type_complexity)]
static LEFTOVER_CACHE: Mutex<
    Option<HashMap<VolumeId, ((Option<i64>, u64), Arc<Vec<(String, String, i64)>>)>>,
> = Mutex::new(None);
/// At most this many cold-folder suggestions per drive.
const COLD_PER_VOLUME: usize = 5;
/// Folder names never suggested for moving: system and application data.
const PROTECTED: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "appdata",
    "$recycle.bin",
    "system volume information",
    "recovery",
    ".git",
    "node_modules",
    ".cache",
    "library",
    "applications",
    "system",
    ".trash",
    ".trashes",
    ".storecat",
    "onedrive",
    "dropbox",
    "google drive",
    "icloud drive",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Urgent,
    Warning,
    Tip,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuggestionAction {
    /// Review this job in Operations; it runs only when the user starts it.
    Job {
        spec: JobSpec,
        /// Drives that must be connected before the job can run.
        needs: Vec<VolumeId>,
    },
    /// Open a view, optionally scoped to a drive or a search.
    View {
        view: String,
        volume_id: Option<VolumeId>,
        query: Option<String>,
        /// A folder to open in the treemap (`view == "storage"`).
        #[serde(default)]
        dir_id: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    /// Stable key, used to dismiss or snooze this exact suggestion.
    pub key: String,
    pub rule: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub bytes: u64,
    pub volume_id: Option<VolumeId>,
    pub action: Option<SuggestionAction>,
    /// Absolute path of the folder concerned, when there is one.
    #[serde(default)]
    pub path: Option<String>,
}

impl Catalog {
    pub fn rule_settings(&self) -> RuleSettings {
        self.setting(RULES_KEY)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub fn set_rule_settings(&self, r: &RuleSettings) -> Result<()> {
        self.set_setting(
            RULES_KEY,
            &serde_json::to_string(r).unwrap_or_else(|_| "{}".into()),
        )
    }

    /// Hide a suggestion for good (`days = None`) or for a number of days.
    pub fn dismiss_suggestion(&self, key: &str, days: Option<u32>) -> Result<()> {
        match days {
            Some(d) => self.master().set_suggestion_state(
                key,
                "snoozed",
                Some(now_unix() + d as i64 * DAY),
            ),
            None => self.master().set_suggestion_state(key, "dismissed", None),
        }
    }

    /// Bring back every dismissed and snoozed suggestion.
    pub fn reset_suggestions(&self) -> Result<()> {
        let keys: Vec<String> = self.master().suggestion_states()?.into_keys().collect();
        for k in keys {
            self.master().set_suggestion_state(&k, "", None)?;
        }
        Ok(())
    }

    /// Evaluate every rule. Cheap enough to call when the Storage view opens: it reads
    /// precomputed folder sizes and walks only the live Desktop and Downloads folders.
    pub fn suggestions(&self) -> Result<Vec<Suggestion>> {
        let rules = self.rule_settings();
        if !rules.enabled {
            return Ok(Vec::new());
        }
        let now = now_unix();
        let vols = self.volumes()?;
        let mut out = Vec::new();

        self.rule_health(&vols, &mut out);
        rule_full(&rules, &vols, &mut out);
        self.rule_sweep(&rules, &vols, now, &mut out);
        for v in &vols {
            if let Err(e) = self.rule_cold(&rules, &vols, v, now, &mut out) {
                log::warn!("cold-folder rule skipped for {}: {e}", v.label);
            }
        }
        self.rule_duplicates(&rules, &mut out);
        for v in &vols {
            if let Err(e) = self.rule_leftovers(v, &mut out) {
                log::warn!("leftover-files rule skipped for {}: {e}", v.label);
            }
        }
        rule_overlap(&vols, &mut out);
        rule_stale(&rules, &vols, now, &mut out);
        rule_roles(&vols, &mut out);

        // The same folder reached through two overlapping catalogues is one suggestion.
        let mut seen_paths = HashSet::new();
        out.retain(|s| {
            s.path
                .as_deref()
                .is_none_or(|p| seen_paths.insert(normalise(p)))
        });

        let states = self.master().suggestion_states()?;
        out.retain(|s| match states.get(&s.key) {
            Some((state, _)) if state == "dismissed" => false,
            Some((state, Some(until))) if state == "snoozed" => *until <= now,
            _ => true,
        });
        out.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.bytes.cmp(&a.bytes)));
        Ok(out)
    }

    fn rule_health(&self, vols: &[Volume], out: &mut Vec<Suggestion>) {
        for v in vols.iter().filter(|v| {
            matches!(v.health, Health::Warning | Health::Unhealthy) && v.total_bytes > 0
        }) {
            let urgent = v.health == Health::Unhealthy;
            let dest = pick_destination(vols, v.total_bytes, v.id, &[Role::Backup, Role::Archive]);
            let action = match (&dest, v.mount_path.as_ref().or(v.last_mount_path.as_ref())) {
                (Some(d), Some(src)) => Some(SuggestionAction::Job {
                    spec: JobSpec {
                        kind: JobKind::Copy,
                        name: format!("Rescue copy of {}", v.label),
                        sources: vec![src.clone()],
                        dest_root: join(
                            d.mount_path
                                .as_ref()
                                .or(d.last_mount_path.as_ref())
                                .unwrap(),
                            &["Storecat backups", &safe_name(&v.label)],
                        ),
                        conflict: ConflictPolicy::Skip,
                        verify: true,
                        checks: Vec::new(),
                    },
                    needs: vec![v.id, d.id],
                }),
                _ => Some(SuggestionAction::View {
                    view: "drives".into(),
                    volume_id: Some(v.id),
                    query: None,
                    dir_id: None,
                }),
            };
            out.push(Suggestion {
            path: None,
                key: format!("health:{}:{}", v.id, v.health.as_str()),
                rule: "health".into(),
                severity: if urgent { Severity::Urgent } else { Severity::Warning },
                title: format!(
                    "{} reports {} health",
                    v.label,
                    if urgent { "failing" } else { "degraded" }
                ),
                body: match &dest {
                    Some(d) => format!(
                        "Windows flagged this drive's S.M.A.R.T. status. Copy its contents to {} while it still reads reliably.",
                        d.label
                    ),
                    None => "Windows flagged this drive's S.M.A.R.T. status. Give another drive the Backup role with enough free space and Storecat will plan a rescue copy.".into(),
                },
                bytes: v.total_bytes,
                volume_id: Some(v.id),
                action,
            });
        }
    }

    fn rule_sweep(
        &self,
        rules: &RuleSettings,
        vols: &[Volume],
        now: i64,
        out: &mut Vec<Suggestion>,
    ) {
        let last = self
            .jobs()
            .unwrap_or_default()
            .into_iter()
            .filter(|j| {
                j.name.starts_with("Desktop + Downloads sweep") && j.status == JobStatus::Done
            })
            .filter_map(|j| j.finished_at)
            .max();
        if let Some(t) = last {
            if now - t < rules.sweep_days as i64 * DAY {
                return;
            }
        }
        // Size the live folders. Desktop and Downloads are small trees, so this is fast.
        let probe = self.sweep_spec("", JobKind::Move);
        if probe.sources.is_empty() {
            return;
        }
        let Ok(plan) = self.plan_job(&probe) else {
            return;
        };
        if (plan.bytes as f64) < rules.sweep_min_gb * GB {
            return;
        }
        let source_vol = probe
            .sources
            .first()
            .and_then(|s| self.volume_for_path(Path::new(s)));
        let dest = pick_destination(
            vols,
            plan.bytes,
            source_vol.unwrap_or(-1),
            &[Role::Archive, Role::Backup],
        );
        let since = match last {
            Some(t) => format!("since your last sweep {} days ago", (now - t) / DAY),
            None => "and have never been swept".into(),
        };
        let (body, action) = match dest {
            Some(d) => {
                let mount = d.mount_path.clone().or(d.last_mount_path.clone()).unwrap_or_default();
                (
                    format!(
                        "Desktop and Downloads hold {} files {since}. Move them into a dated archive folder on {}, verified before anything is removed.",
                        count(plan.files), d.label
                    ),
                    Some(SuggestionAction::Job {
                        spec: self.sweep_spec(&mount, JobKind::Move),
                        needs: vec![d.id],
                    }),
                )
            }
            None => (
                format!(
                    "Desktop and Downloads hold {} files {since}. Give a drive the Archive role to get a one-click sweep.",
                    count(plan.files)
                ),
                Some(SuggestionAction::View {
                    view: "operations".into(),
                    volume_id: None,
                    query: None,
                dir_id: None,
                }),
            ),
        };
        out.push(Suggestion {
            path: None,
            key: format!("sweep:{}", quarter(now)),
            rule: "sweep".into(),
            severity: Severity::Tip,
            title: "Time for the Desktop + Downloads sweep".into(),
            body,
            bytes: plan.bytes,
            volume_id: None,
            action,
        });
    }

    fn rule_cold(
        &self,
        rules: &RuleSettings,
        vols: &[Volume],
        v: &Volume,
        now: i64,
        out: &mut Vec<Suggestion>,
    ) -> Result<()> {
        let fast = matches!(v.media_type, MediaType::Ssd | MediaType::Nvme);
        if !fast || matches!(v.role, Role::Archive | Role::Backup) || v.last_scan_at.is_none() {
            return Ok(());
        }
        self.ensure_dir_sizes(v.id)?;
        let min = (rules.cold_min_gb * GB) as i64;
        let before = now - rules.cold_days as i64 * DAY;
        let db = self.volume_db(v.id)?;
        let mut stmt = db.conn().prepare(
            "SELECT d.id, d.parent_id, d.path, s.bytes, s.files, s.newest
             FROM dir_sizes s JOIN dirs d ON d.id = s.dir_id
             WHERE s.bytes >= ?1 AND s.newest < ?2 AND s.newest > 0 AND d.parent_id IS NOT NULL
             ORDER BY s.bytes DESC",
        )?;
        let rows: Vec<(i64, Option<i64>, String, i64, i64, i64)> = stmt
            .query_map(rusqlite::params![min, before], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let cold: HashSet<i64> = rows.iter().map(|r| r.0).collect();
        let base = v
            .mount_path
            .clone()
            .or(v.last_mount_path.clone())
            .unwrap_or_default();
        let mut picked = 0;
        for (dir_id, parent, rel, bytes, files, newest) in rows {
            if parent.is_some_and(|p| cold.contains(&p)) {
                continue; // a cold ancestor covers it
            }
            if rel
                .split(['/', '\\'])
                .any(|seg| PROTECTED.contains(&seg.to_lowercase().as_str()))
            {
                continue;
            }
            let bytes = bytes as u64;
            let src = join(&base, &[&rel]);
            let abs = src.clone();
            let dest = pick_destination(vols, bytes, v.id, &[Role::Archive, Role::Backup]);
            let parent_rel = Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = Path::new(&rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel.clone());
            let years = ((now - newest) as f64 / (365.25 * DAY as f64)).floor() as i64;
            let files = count(files as u64);
            let age = if years >= 2 {
                format!("{years} years")
            } else {
                format!("{} months", ((now - newest) / (30 * DAY)).max(1))
            };
            let (body, action) = match dest {
                Some(d) => {
                    let dmount = d.mount_path.clone().or(d.last_mount_path.clone()).unwrap_or_default();
                    let mut parts = vec!["Storecat archive".to_string(), safe_name(&v.label)];
                    parts.extend(
                        parent_rel
                            .split(['/', '\\'])
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                    (
                        format!(
                            "{files} files untouched for {age} on fast storage. Move them to {} and keep {} free on {}.",
                            d.label,
                            human(bytes),
                            v.label
                        ),
                        Some(SuggestionAction::Job {
                            spec: JobSpec {
                                kind: JobKind::Move,
                                name: format!("Archive {name} from {}", v.label),
                                sources: vec![src],
                                dest_root: join(&dmount, &refs),
                                conflict: ConflictPolicy::Rename,
                                verify: true,
                                checks: Vec::new(),
                            },
                            needs: vec![v.id, d.id],
                        }),
                    )
                }
                None => (
                    format!(
                        "{files} files untouched for {age} on fast storage. Give a drive with enough free space the Archive role to move them there."
                    ),
                    Some(SuggestionAction::View {
                        view: "storage".into(),
                        volume_id: Some(v.id),
                        query: None,
                dir_id: Some(dir_id),
                    }),
                ),
            };
            out.push(Suggestion {
                path: Some(abs),
                key: format!("cold:{}:{}", v.id, rel.to_lowercase()),
                rule: "cold".into(),
                severity: Severity::Tip,
                title: format!("{name} hasn't changed in {age}"),
                body,
                bytes,
                volume_id: Some(v.id),
                action,
            });
            picked += 1;
            if picked >= COLD_PER_VOLUME {
                break;
            }
        }
        Ok(())
    }

    /// Files macOS and Windows leave on drives (`._name`, `.DS_Store`, `Thumbs.db`):
    /// offer one reviewable job that recycles them.
    fn rule_leftovers(&self, v: &Volume, out: &mut Vec<Suggestion>) -> Result<()> {
        let Some(mount) = v.mount_path.clone().or(v.last_mount_path.clone()) else {
            return Ok(());
        };
        if !self.dirs.volume_db(v.id).exists() {
            return Ok(());
        }
        // Reading every name takes most of a second on a big drive: remember the answer
        // until the drive is scanned again.
        let stamp = (v.last_scan_at, v.file_count);
        let cached = LEFTOVER_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(&v.id))
            .filter(|(s, _)| *s == stamp)
            .map(|(_, rows)| Arc::clone(rows));
        let found = match cached {
            Some(rows) => rows,
            None => {
                let db = self.volume_db(v.id)?;
                let mut stmt = db.conn().prepare(
                    "SELECT d.path, f.name, f.size FROM files f JOIN dirs d ON d.id = f.dir_id
                     WHERE substr(f.name, 1, 2) = '._'
                        OR lower(f.name) IN ('.ds_store', 'thumbs.db', 'ehthumbs.db',
                                             'ehthumbs_vista.db', '.localized')",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<Vec<(String, String, i64)>>>()?;
                let rows = Arc::new(rows);
                LEFTOVER_CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert_with(HashMap::new)
                    .insert(v.id, (stamp, Arc::clone(&rows)));
                rows
            }
        };
        let mut sources = Vec::new();
        let mut checks = Vec::new();
        let (mut bytes, mut mac, mut windows) = (0u64, 0u64, 0u64);
        for (dir, name, size) in found.iter() {
            let (dir, name, size) = (dir.as_str(), name.as_str(), *size);
            if !crate::places::is_cleanable_leftover(name) {
                continue;
            }
            let apple_double = name.starts_with("._");
            if apple_double
                || name.eq_ignore_ascii_case(".DS_Store")
                || name.eq_ignore_ascii_case(".localized")
            {
                mac += 1;
            } else {
                windows += 1;
            }
            bytes += size.max(0) as u64;
            sources.push(crate::catalog::join_abs(&mount, dir, name));
            checks.push(if apple_double {
                crate::ops::CHECK_APPLEDOUBLE.to_string()
            } else {
                String::new()
            });
        }
        let n = sources.len() as u64;
        if n < LEFTOVERS_MIN {
            return Ok(());
        }
        let what = match (mac, windows) {
            (_, 0) => "Mac leftover files",
            (0, _) => "Windows thumbnail caches",
            _ => "Mac and Windows leftover files",
        };
        out.push(Suggestion {
            path: None,
            key: format!("leftovers:{}:{}", v.id, n / 100),
            rule: "leftovers".into(),
            severity: Severity::Tip,
            title: format!("{} {what} ({}) on {}", count(n), human(bytes), v.label),
            body: "macOS leaves ._ and .DS_Store files on drives it writes to; Windows leaves                    Thumbs.db. Nothing here uses them and search already hides them. Each ._                    file is checked to be macOS metadata before it goes to the Recycle Bin."
                .into(),
            bytes,
            volume_id: Some(v.id),
            action: Some(SuggestionAction::Job {
                spec: JobSpec {
                    kind: JobKind::Delete,
                    name: format!("Remove {} {what} from {}", count(n), v.label),
                    sources,
                    dest_root: String::new(),
                    conflict: ConflictPolicy::Skip,
                    verify: false,
                    checks,
                },
                needs: vec![v.id],
            }),
        });
        Ok(())
    }

    fn rule_duplicates(&self, rules: &RuleSettings, out: &mut Vec<Suggestion>) {
        let Ok(d) = self.duplicate_summary() else {
            return;
        };
        if (d.waste as f64) < rules.duplicates_min_gb * GB {
            return;
        }
        out.push(Suggestion {
            path: None,
            key: format!("duplicates:{}", d.waste / (10 * GB as u64).max(1)),
            rule: "duplicates".into(),
            severity: Severity::Tip,
            title: format!("{} is taken up by duplicate copies", human(d.waste)),
            body: format!(
                "{} of your files exist more than once across your drives. Pick the copy to keep and the others go to the Recycle Bin. Copies inside app folders are left alone.",
                count(d.files)
            ),
            bytes: d.waste,
            volume_id: None,
            action: Some(SuggestionAction::View {
                view: "duplicates".into(),
                volume_id: None,
                query: None,
                dir_id: None,
            }),
        });
    }
}

fn rule_full(rules: &RuleSettings, vols: &[Volume], out: &mut Vec<Suggestion>) {
    // Catalogued folders report the free space of the disk they live on, so several of
    // them can describe one physical drive. Warn once per drive: a catalogued drive wins,
    // otherwise the folder with the shortest path. Capacity identifies the disk, as in
    // the Storage view's totals.
    let mut candidates: Vec<&Volume> = vols
        .iter()
        .filter(|v| v.capacity > 0 && v.media_type != MediaType::Optical)
        .collect();
    candidates.sort_by_key(|v| {
        (
            v.source != VolumeSource::Device,
            v.mount_path.as_deref().map_or(usize::MAX, str::len),
        )
    });
    let mut seen = HashSet::new();
    for v in candidates {
        let folder = v.source != VolumeSource::Device;
        if folder
            && vols
                .iter()
                .any(|o| o.source == VolumeSource::Device && o.online && o.capacity == v.capacity)
        {
            continue;
        }
        if !seen.insert(if folder {
            (1, v.capacity)
        } else {
            (0, v.id as u64)
        }) {
            continue;
        }
        let used = v.capacity.saturating_sub(v.free);
        let pct = (used as f64 / v.capacity as f64 * 100.0).round() as u32;
        if pct < rules.full_percent {
            continue;
        }
        let name = if folder {
            format!("The drive holding {}", v.label)
        } else {
            v.label.clone()
        };
        out.push(Suggestion {
            path: None,
            key: format!("full:{}:{}", v.id, pct / 5 * 5),
            rule: "full".into(),
            severity: Severity::Warning,
            title: format!("{name} is {pct}% full"),
            body: format!(
                "Only {} left. The treemap shows which folders take the space.",
                human(v.free)
            ),
            bytes: used,
            volume_id: Some(v.id),
            action: Some(SuggestionAction::View {
                view: "storage".into(),
                volume_id: Some(v.id),
                query: None,
                dir_id: None,
            }),
        });
    }
}

/// Catalogued volumes whose root lies inside another catalogued volume.
pub fn overlaps(vols: &[Volume]) -> Vec<(VolumeId, VolumeId)> {
    let root = |v: &Volume| {
        v.mount_path
            .clone()
            .or(v.last_mount_path.clone())
            .map(|p| normalise(&p))
    };
    let mut out = Vec::new();
    for inner in vols {
        let Some(ip) = root(inner) else { continue };
        for outer in vols.iter().filter(|o| o.id != inner.id) {
            let Some(op) = root(outer) else { continue };
            // Only compare drives that are online together or folder paths: a drive letter
            // reused by a different, offline drive is not an overlap.
            if outer.source == VolumeSource::Device && !outer.online {
                continue;
            }
            if ip.len() > op.len() && ip.starts_with(&op) && ip[op.len()..].starts_with('/') {
                out.push((inner.id, outer.id));
            }
        }
    }
    out
}

fn normalise(p: &str) -> String {
    let mut s = p.replace('\\', "/").to_lowercase();
    while s.ends_with('/') {
        s.pop();
    }
    s
}

fn rule_overlap(vols: &[Volume], out: &mut Vec<Suggestion>) {
    for (inner, outer) in overlaps(vols) {
        let (Some(i), Some(o)) = (
            vols.iter().find(|v| v.id == inner),
            vols.iter().find(|v| v.id == outer),
        ) else {
            continue;
        };
        out.push(Suggestion {
            path: None,
            key: format!("overlap:{inner}:{outer}"),
            rule: "overlap".into(),
            severity: Severity::Info,
            title: format!("{} is catalogued twice", i.label),
            body: format!(
                "It sits inside {}, so its files show up twice in search. Remove one of them in Drives unless you want both.",
                o.label
            ),
            bytes: i.total_bytes,
            volume_id: Some(inner),
            action: Some(SuggestionAction::View {
                view: "drives".into(),
                volume_id: Some(inner),
                query: None,
                dir_id: None,
            }),
        });
    }
}

fn rule_stale(rules: &RuleSettings, vols: &[Volume], now: i64, out: &mut Vec<Suggestion>) {
    for v in vols.iter().filter(|v| !v.online) {
        let Some(t) = v.last_scan_at else { continue };
        let days = (now - t) / DAY;
        if days < rules.stale_days as i64 {
            continue;
        }
        out.push(Suggestion {
            path: None,
            key: format!("stale:{}:{}", v.id, t),
            rule: "stale".into(),
            severity: Severity::Info,
            title: format!("{}'s catalogue is {days} days old", v.label),
            body: "Plug it in and Storecat refreshes the catalogue in the background.".into(),
            bytes: 0,
            volume_id: Some(v.id),
            action: None,
        });
    }
}

fn rule_roles(vols: &[Volume], out: &mut Vec<Suggestion>) {
    let unassigned = vols.iter().filter(|v| v.role == Role::Unassigned).count();
    let has_target = vols
        .iter()
        .any(|v| matches!(v.role, Role::Archive | Role::Backup));
    if unassigned == 0 || has_target {
        return;
    }
    out.push(Suggestion {
            path: None,
        key: "roles".into(),
        rule: "roles".into(),
        severity: Severity::Info,
        title: "Give your drives roles".into(),
        body: format!(
            "{unassigned} of {} drives have no role. Mark which ones are for archive and backup, and Storecat can suggest where things should live.",
            vols.len()
        ),
        bytes: 0,
        volume_id: None,
        action: Some(SuggestionAction::View {
            view: "drives".into(),
            volume_id: None,
            query: None,
                dir_id: None,
        }),
    });
}

/// The best drive to receive `bytes`: a preferred role first (in order), then any spinning
/// disk that isn't the system or working drive. Connected drives win ties, then most free.
fn pick_destination<'a>(
    vols: &'a [Volume],
    bytes: u64,
    exclude: VolumeId,
    roles: &[Role],
) -> Option<&'a Volume> {
    let need = bytes + bytes / 20 + (1 << 30);
    let rank = |v: &Volume| -> Option<usize> {
        if let Some(i) = roles.iter().position(|r| *r == v.role) {
            return Some(i);
        }
        let spare_hdd =
            v.media_type == MediaType::Hdd && !matches!(v.role, Role::System | Role::Working);
        spare_hdd.then_some(roles.len())
    };
    vols.iter()
        .filter(|v| v.id != exclude && v.capacity > 0 && v.free >= need)
        .filter(|v| v.media_type != MediaType::Optical)
        .filter(|v| v.mount_path.is_some() || v.last_mount_path.is_some())
        .filter_map(|v| rank(v).map(|r| (r, v)))
        .min_by(|(ra, a), (rb, b)| {
            ra.cmp(rb)
                .then(b.online.cmp(&a.online))
                .then(b.free.cmp(&a.free))
        })
        .map(|(_, v)| v)
}

fn join(base: &str, parts: &[&str]) -> String {
    let mut p = std::path::PathBuf::from(base);
    for part in parts {
        for seg in part.split(['/', '\\']).filter(|s| !s.is_empty()) {
            p.push(seg);
        }
    }
    p.to_string_lossy().into_owned()
}

/// A drive label made safe for use as a folder name.
fn safe_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = cleaned.trim().trim_end_matches('.').to_string();
    if t.is_empty() {
        "Drive".into()
    } else {
        t
    }
}

fn quarter(now: i64) -> String {
    let dt =
        time::OffsetDateTime::from_unix_timestamp(now).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{}-q{}", dt.year(), (dt.month() as u8 - 1) / 3 + 1)
}

/// 17513 -> "17,513".
fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn human(b: u64) -> String {
    let f = b as f64;
    if f >= GB * 1024.0 {
        format!("{:.1} TB", f / GB / 1024.0)
    } else if f >= GB {
        format!("{:.1} GB", f / GB)
    } else if f >= 100.0 * 1024.0 * 1024.0 {
        format!("{:.0} MB", f / 1024.0 / 1024.0)
    } else if f >= 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", (f / 1024.0).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanOptions;

    fn vol(
        id: VolumeId,
        label: &str,
        media: MediaType,
        role: Role,
        free: u64,
        mount: &str,
    ) -> Volume {
        Volume {
            id,
            identity: format!("t{id}"),
            source: VolumeSource::Device,
            label: label.into(),
            fs: "ntfs".into(),
            capacity: free * 2,
            free,
            media_type: media,
            role,
            notes: String::new(),
            location: String::new(),
            last_scan_at: Some(0),
            last_seen_at: None,
            last_mount_path: Some(mount.into()),
            file_count: 0,
            dir_count: 0,
            total_bytes: 0,
            scan_seconds: 0.0,
            created_at: 0,
            photo: false,
            bus: String::new(),
            model: String::new(),
            health: Health::Unknown,
            health_at: None,
            online: true,
            mount_path: Some(mount.into()),
        }
    }

    #[test]
    fn destination_prefers_role_then_online_then_free() {
        let g = 1u64 << 30;
        let vols = vec![
            vol(1, "SSD", MediaType::Ssd, Role::Working, 100 * g, "C:\\"),
            vol(
                2,
                "Big HDD",
                MediaType::Hdd,
                Role::Unassigned,
                900 * g,
                "E:\\",
            ),
            vol(3, "Archive", MediaType::Hdd, Role::Archive, 200 * g, "F:\\"),
            vol(
                4,
                "Tiny archive",
                MediaType::Hdd,
                Role::Archive,
                2 * g,
                "G:\\",
            ),
        ];
        let d = pick_destination(&vols, 50 * g, 1, &[Role::Archive, Role::Backup]).unwrap();
        assert_eq!(d.id, 3);
        // Too big for the archive drive: falls back to the spare spinning disk.
        let d = pick_destination(&vols, 300 * g, 1, &[Role::Archive, Role::Backup]).unwrap();
        assert_eq!(d.id, 2);
        assert!(pick_destination(&vols, 5000 * g, 1, &[Role::Archive]).is_none());
    }

    #[test]
    fn overlap_detection() {
        let g = 1u64 << 30;
        let mut a = vol(
            1,
            "Home",
            MediaType::Ssd,
            Role::Working,
            g,
            "C:\\Users\\sam",
        );
        a.source = VolumeSource::Folder;
        let mut b = vol(
            2,
            "Music",
            MediaType::Ssd,
            Role::Working,
            g,
            "C:\\Users\\sam\\Music",
        );
        b.source = VolumeSource::Folder;
        let c = vol(
            3,
            "Samsung",
            MediaType::Ssd,
            Role::Working,
            g,
            "C:\\Users\\samantha",
        );
        let found = overlaps(&[a, b, c]);
        assert_eq!(found, vec![(2, 1)]);
    }

    #[test]
    fn full_drive_is_one_suggestion_per_disk() {
        let g = 1u64 << 30;
        let full = |mut v: Volume, folder: bool| {
            v.capacity = 1000 * g;
            v.free = 50 * g;
            if folder {
                v.source = VolumeSource::Folder;
            }
            v
        };
        let rules = RuleSettings::default();
        let folders = vec![
            full(
                vol(
                    1,
                    "Music",
                    MediaType::Nvme,
                    Role::Unassigned,
                    g,
                    "C:\\Users\\sam\\Music",
                ),
                true,
            ),
            full(
                vol(
                    2,
                    "sam",
                    MediaType::Nvme,
                    Role::Unassigned,
                    g,
                    "C:\\Users\\sam",
                ),
                true,
            ),
            full(
                vol(
                    3,
                    "Videos",
                    MediaType::Nvme,
                    Role::Unassigned,
                    g,
                    "C:\\Users\\sam\\Videos",
                ),
                true,
            ),
        ];
        let mut out = Vec::new();
        rule_full(&rules, &folders, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "The drive holding sam is 95% full");

        // Once the disk itself is catalogued, it speaks for its folders.
        let mut all = folders;
        all.push(full(
            vol(4, "Windows-SSD", MediaType::Nvme, Role::System, g, "C:\\"),
            false,
        ));
        let mut out = Vec::new();
        rule_full(&rules, &all, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].volume_id, Some(4));
    }

    #[test]
    fn cold_folders_become_move_suggestions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ssd");
        std::fs::create_dir_all(root.join("Projects/2019/raw")).unwrap();
        std::fs::create_dir_all(root.join("Projects/current")).unwrap();
        std::fs::create_dir_all(root.join("AppData/old")).unwrap();
        for p in [
            "Projects/2019/raw/a.mov",
            "Projects/2019/b.mov",
            "AppData/old/c.bin",
        ] {
            std::fs::write(root.join(p), vec![1u8; 4096]).unwrap();
        }
        std::fs::write(root.join("Projects/current/new.txt"), b"x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(800 * 86_400);
        for p in [
            "Projects/2019/raw/a.mov",
            "Projects/2019/b.mov",
            "AppData/old/c.bin",
        ] {
            std::fs::File::options()
                .write(true)
                .open(root.join(p))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();

        let cat = Catalog::open(crate::DataDirs::at(tmp.path().join("data"))).unwrap();
        let v = cat.add_folder(&root, Some("Fast")).unwrap();
        cat.update_volume(
            v.id,
            &VolumePatch {
                media_type: Some(MediaType::Ssd),
                role: Some(Role::Working),
                ..Default::default()
            },
        )
        .unwrap();
        let a = cat.add_folder(&archive, Some("Vault")).unwrap();
        cat.update_volume(
            a.id,
            &VolumePatch {
                media_type: Some(MediaType::Hdd),
                role: Some(Role::Archive),
                ..Default::default()
            },
        )
        .unwrap();
        let opts = ScanOptions {
            threads: 2,
            ..Default::default()
        };
        cat.scan(v.id, &opts, &|_| {}).unwrap();
        cat.refresh_folders(&|_| false).unwrap();
        cat.set_rule_settings(&RuleSettings {
            cold_min_gb: 4096.0 / GB,
            sweep_min_gb: 1e9,
            ..Default::default()
        })
        .unwrap();

        let s = cat.suggestions().unwrap();
        let cold: Vec<&Suggestion> = s.iter().filter(|s| s.rule == "cold").collect();
        assert_eq!(cold.len(), 1, "{s:#?}");
        assert!(cold[0].title.starts_with("2019"), "{}", cold[0].title);
        let Some(SuggestionAction::Job { spec, needs }) = &cold[0].action else {
            panic!("expected a job");
        };
        assert_eq!(spec.kind, JobKind::Move);
        assert!(spec.sources[0].ends_with("2019"));
        assert!(spec.dest_root.contains("Storecat archive"));
        assert!(spec.dest_root.ends_with("Projects"), "{}", spec.dest_root);
        assert_eq!(needs, &vec![v.id, a.id]);

        // Snoozed suggestions disappear, and come back after a reset.
        cat.dismiss_suggestion(&cold[0].key, Some(30)).unwrap();
        assert!(cat.suggestions().unwrap().iter().all(|s| s.rule != "cold"));
        cat.reset_suggestions().unwrap();
        assert!(cat.suggestions().unwrap().iter().any(|s| s.rule == "cold"));
    }

    #[test]
    fn leftover_files_become_one_reviewable_job() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let music = tree.path().join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let mut apple = crate::places::APPLEDOUBLE_MAGIC.to_vec();
        apple.extend_from_slice(&[0u8; 4092]);
        for i in 0..11 {
            std::fs::write(music.join(format!("song{i}.mp3")), b"tune").unwrap();
            std::fs::write(music.join(format!("._song{i}.mp3")), &apple).unwrap();
        }
        std::fs::write(music.join(".DS_Store"), b"finder").unwrap();
        std::fs::write(music.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        let cat = Catalog::open(crate::paths::DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Stick")).unwrap();
        cat.scan(v.id, &crate::scan::ScanOptions::default(), &|_| {})
            .unwrap();

        let s = cat.suggestions().unwrap();
        let s = s.iter().find(|s| s.rule == "leftovers").unwrap();
        assert_eq!(s.title, "12 Mac leftover files (44 KB) on Stick");
        let Some(SuggestionAction::Job { spec, needs }) = &s.action else {
            panic!("expected a job");
        };
        assert_eq!(needs, &vec![v.id]);
        assert_eq!(spec.kind, JobKind::Delete);
        assert_eq!(spec.sources.len(), 12);
        assert!(spec.sources.iter().all(|p| !p.ends_with("desktop.ini")));
        for (src, check) in spec.sources.iter().zip(&spec.checks) {
            let apple_double = std::path::Path::new(src)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("._");
            assert_eq!(check == crate::ops::CHECK_APPLEDOUBLE, apple_double);
        }
    }
}
