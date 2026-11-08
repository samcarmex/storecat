//! Platform layer: everything that differs between Windows, macOS and Linux.
//!
//! Volume detection with a stable identity, a drive profile (bus, model, SSD/HDD/NVMe,
//! optical) and the health the operating system reports for the physical disk.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use storecat_core::model::{DetectedVolume, Health, MediaType};
use sysinfo::{DiskKind, Disks};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Name of the current platform, used in diagnostics.
pub fn name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// What the OS can tell about the device behind a mount point. Cheap to ask once, so it
/// is cached per mount and refreshed only when a mount appears or the cache goes stale.
#[derive(Debug, Clone, Default)]
pub struct DriveProfile {
    pub bus: String,
    pub model: String,
    pub media: Option<MediaType>,
    pub health: Health,
}

struct Cached {
    key: String,
    profile: DriveProfile,
    at: Instant,
}

/// Health is re-read at most this often for a drive that stays connected.
const PROFILE_TTL: Duration = Duration::from_secs(60 * 60);

static PROFILES: Mutex<Vec<Cached>> = Mutex::new(Vec::new());

/// Enumerate the volumes mounted right now.
///
/// Identity is `fs:serial:capacity` where the filesystem exposes a serial (Windows volume
/// serial, which travels with the drive between machines), otherwise a fallback built from
/// the mount point and capacity, prefixed so the two can never collide.
pub fn detect_volumes() -> Vec<DetectedVolume> {
    let disks = Disks::new_with_refreshed_list();
    let mut out = Vec::new();
    for disk in disks.list() {
        let mount = disk.mount_point().to_string_lossy().into_owned();
        if mount.is_empty() {
            continue;
        }
        let capacity = disk.total_space();
        if capacity == 0 {
            continue;
        }
        let fs = disk.file_system().to_string_lossy().into_owned();
        if is_pseudo_fs(&fs, &mount) {
            continue;
        }
        let label = disk.name().to_string_lossy().trim().to_string();
        let media_type = match disk.kind() {
            DiskKind::SSD => MediaType::Ssd,
            DiskKind::HDD => MediaType::Hdd,
            DiskKind::Unknown(_) => MediaType::Unknown,
        };
        let (serial, guid) = volume_ids(&mount, &label);
        let identity = match serial {
            Some(s) => format!("{}:{}:{}", fs.to_lowercase(), s, capacity),
            None => format!("mount:{}:{}", mount.to_lowercase(), capacity),
        };
        let label = display_label(&label, &mount);
        out.push(DetectedVolume {
            identity,
            mount_path: mount,
            label,
            fs,
            capacity,
            free: disk.available_space(),
            media_type,
            removable: disk.is_removable(),
            guid,
            bus: String::new(),
            model: String::new(),
            health: Health::Unknown,
        });
    }
    #[cfg(windows)]
    out.extend(windows::optical_volumes(&out));

    // One entry per volume: bind mounts (Linux, WSL) and extra mount points (Windows
    // folder mounts) expose the same filesystem more than once. Keep the shortest path.
    out.sort_by_key(|v| v.mount_path.len());
    let mut seen = std::collections::HashSet::new();
    out.retain(|v| seen.insert(v.identity.clone()));

    apply_profiles(&mut out);
    out.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
    out
}

/// Fill bus / model / health from the cache, asking the OS only for new or stale mounts.
fn apply_profiles(vols: &mut [DetectedVolume]) {
    let mut cache = PROFILES.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|c| vols.iter().any(|v| profile_key(v) == c.key));
    let stale: Vec<usize> = vols
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            let key = profile_key(v);
            !cache
                .iter()
                .any(|c| c.key == key && c.at.elapsed() < PROFILE_TTL)
        })
        .map(|(i, _)| i)
        .collect();
    if !stale.is_empty() {
        let mounts: Vec<&str> = stale.iter().map(|&i| vols[i].mount_path.as_str()).collect();
        let profiles = read_profiles(&mounts);
        for (&i, profile) in stale.iter().zip(profiles) {
            let key = profile_key(&vols[i]);
            cache.retain(|c| c.key != key);
            cache.push(Cached {
                key,
                profile,
                at: Instant::now(),
            });
        }
    }
    for v in vols.iter_mut() {
        let key = profile_key(v);
        if let Some(c) = cache.iter().find(|c| c.key == key) {
            let p = &c.profile;
            v.bus = p.bus.clone();
            v.model = p.model.clone();
            v.health = p.health;
            // USB enclosures often don't say whether the disk spins; the name usually does.
            if let Some(m) = p.media.or_else(|| media_from_model(&p.model)) {
                // NVMe and optical are more specific than the SSD/HDD guess.
                if v.media_type == MediaType::Unknown
                    || matches!(m, MediaType::Nvme | MediaType::Optical)
                {
                    v.media_type = m;
                }
            }
        }
    }
}

/// SSD or HDD from a drive's model name, for enclosures that report neither ("Seagate
/// Expansion HDD", "Samsung Portable SSD T7").
fn media_from_model(model: &str) -> Option<MediaType> {
    let words: Vec<String> = model
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.to_ascii_uppercase())
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    if has("SSD") || has("NVME") {
        Some(MediaType::Ssd)
    } else if has("HDD") {
        Some(MediaType::Hdd)
    } else {
        None
    }
}

fn profile_key(v: &DetectedVolume) -> String {
    format!("{}|{}", v.mount_path, v.identity)
}

fn read_profiles(mounts: &[&str]) -> Vec<DriveProfile> {
    #[cfg(windows)]
    {
        windows::profiles(mounts)
    }
    #[cfg(target_os = "linux")]
    {
        mounts.iter().map(|m| linux::profile(m)).collect()
    }
    #[cfg(target_os = "macos")]
    {
        mounts.iter().map(|m| macos::profile(m)).collect()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        vec![DriveProfile::default(); mounts.len()]
    }
}

/// Whether `path` lives on a network share (UNC path or mapped drive / network mount).
pub fn is_remote_path(path: &str) -> bool {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    #[cfg(windows)]
    {
        windows::is_remote_drive(path)
    }
    #[cfg(target_os = "linux")]
    {
        linux::is_remote_mount(path)
    }
    #[cfg(target_os = "macos")]
    {
        macos::is_remote_mount(path)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Filesystems that are not storage the user would catalogue (Linux/macOS mostly).
fn is_pseudo_fs(fs: &str, mount: &str) -> bool {
    const PSEUDO: &[&str] = &[
        "tmpfs",
        "devtmpfs",
        "overlay",
        "squashfs",
        "proc",
        "sysfs",
        "devfs",
        "autofs",
        "ramfs",
        "efivarfs",
        "fuse.portal",
        "nullfs",
    ];
    let fs = fs.to_ascii_lowercase();
    if PSEUDO.contains(&fs.as_str()) {
        return true;
    }
    if cfg!(target_os = "macos") {
        // APFS system/VM/preboot volumes; the data volume is what users see as "Macintosh HD".
        return mount.starts_with("/System/Volumes/") && mount != "/System/Volumes/Data";
    }
    if cfg!(target_os = "linux") {
        return mount.starts_with("/snap/")
            || mount.starts_with("/boot")
            || mount.starts_with("/usr/lib/wsl")
            || mount == "/run";
    }
    false
}

fn display_label(label: &str, mount: &str) -> String {
    let label = unescape_octal(label);
    if !label.is_empty() && !label.starts_with("/dev/") {
        return label;
    }
    let trimmed = mount.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        "Root".to_string()
    } else if let Some(name) = trimmed.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()) {
        if cfg!(windows) {
            format!("Drive {trimmed}")
        } else {
            name.to_string()
        }
    } else {
        format!("Drive {trimmed}")
    }
}

/// `/proc/mounts` escapes spaces and backslashes as `\040`, `\134`; undo that.
fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (volume serial / UUID, volume GUID path) when the OS exposes them.
fn volume_ids(mount: &str, device: &str) -> (Option<String>, Option<String>) {
    #[cfg(windows)]
    {
        let _ = device;
        windows::volume_ids(mount)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = mount;
        linux::volume_uuid(device)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = device;
        macos::volume_uuid(mount)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (mount, device);
        (None, None)
    }
}

/// `key: value` lines into a map, for modules that shell out to OS tools.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_kv(text: &str, sep: char) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(sep)?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_from_model_names() {
        assert_eq!(
            media_from_model("Seagate Expansion HDD"),
            Some(MediaType::Hdd)
        );
        assert_eq!(
            media_from_model("Samsung Portable SSD T7"),
            Some(MediaType::Ssd)
        );
        assert_eq!(
            media_from_model("WD_BLACK SN850X NVMe"),
            Some(MediaType::Ssd)
        );
        assert_eq!(media_from_model("ST8000DM004-2CX188"), None);
        assert_eq!(media_from_model("SSDPEKKW512G8"), None);
    }

    #[test]
    fn detects_at_least_the_system_volume() {
        let vols = detect_volumes();
        assert!(!vols.is_empty());
        for v in &vols {
            assert!(!v.identity.is_empty());
            assert!(v.capacity > 0);
        }
        let ids: std::collections::HashSet<&str> =
            vols.iter().map(|v| v.identity.as_str()).collect();
        assert_eq!(ids.len(), vols.len(), "identities must be unique");
    }

    #[test]
    fn profiles_are_cached() {
        let a = detect_volumes();
        let t = Instant::now();
        let b = detect_volumes();
        assert_eq!(a.len(), b.len());
        // The second call must not shell out again for health.
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "{:?}",
            t.elapsed()
        );
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.bus, y.bus);
            assert_eq!(x.health, y.health);
        }
    }

    #[test]
    fn mount_labels_are_unescaped() {
        assert_eq!(unescape_octal(r"C:\134"), r"C:\");
        assert_eq!(unescape_octal(r"My\040Drive"), "My Drive");
        assert_eq!(unescape_octal("plain"), "plain");
    }

    #[test]
    fn unc_paths_are_remote() {
        assert!(is_remote_path(r"\\nas\media"));
        assert!(is_remote_path("//nas/media"));
    }
}
