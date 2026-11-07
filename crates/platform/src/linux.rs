//! Linux: filesystem UUIDs from /dev/disk/by-uuid, drive profile from sysfs, network
//! mounts from /proc/mounts. Health needs root (smartctl), so it stays unknown here.

use std::fs;
use std::path::{Path, PathBuf};

use storecat_core::model::MediaType;

use crate::DriveProfile;

const REMOTE_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "fuse.sshfs",
    "9p",
    "afs",
    "ceph",
    "glusterfs",
    "fuse.rclone",
];

/// (filesystem UUID, None) for a block device such as `/dev/sdb1`.
pub(crate) fn volume_uuid(device: &str) -> (Option<String>, Option<String>) {
    let Ok(target) = fs::canonicalize(device) else {
        return (None, None);
    };
    let Ok(rd) = fs::read_dir("/dev/disk/by-uuid") else {
        return (None, None);
    };
    for e in rd.flatten() {
        if fs::canonicalize(e.path()).ok().as_deref() == Some(target.as_path()) {
            return (Some(e.file_name().to_string_lossy().into_owned()), None);
        }
    }
    (None, None)
}

/// (device, mount point, fs type) rows of /proc/mounts.
fn mounts() -> Vec<(String, String, String)> {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let dev = it.next()?;
            let mnt = it.next()?.replace("\\040", " ");
            let fs = it.next()?;
            Some((dev.to_string(), mnt, fs.to_string()))
        })
        .collect()
}

/// The mount whose path is the longest prefix of `path`.
fn mount_for(path: &str) -> Option<(String, String, String)> {
    mounts()
        .into_iter()
        .filter(|(_, m, _)| Path::new(path).starts_with(m))
        .max_by_key(|(_, m, _)| m.len())
}

pub(crate) fn is_remote_mount(path: &str) -> bool {
    mount_for(path).is_some_and(|(_, _, fs)| REMOTE_FS.contains(&fs.as_str()))
}

/// The whole-disk sysfs directory for a partition device (sda1 -> /sys/block/sda).
fn disk_dir(device: &str) -> Option<PathBuf> {
    let name = Path::new(device)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let class = PathBuf::from("/sys/class/block").join(&name);
    let real = fs::canonicalize(&class).ok()?;
    if real.join("partition").exists() {
        real.parent().map(Path::to_path_buf)
    } else {
        Some(real)
    }
}

pub(crate) fn profile(mount: &str) -> DriveProfile {
    let mut p = DriveProfile::default();
    let Some((device, _, fs)) = mount_for(mount) else {
        return p;
    };
    if REMOTE_FS.contains(&fs.as_str()) {
        p.bus = "network".into();
        p.media = Some(MediaType::Network);
        return p;
    }
    if fs == "iso9660" || fs == "udf" {
        p.bus = "optical".into();
        p.media = Some(MediaType::Optical);
        return p;
    }
    let Some(disk) = disk_dir(&device) else {
        return p;
    };
    let name = disk
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let real = disk.to_string_lossy().into_owned();
    p.bus = if name.starts_with("nvme") {
        "nvme"
    } else if name.starts_with("mmcblk") {
        "sd"
    } else if real.contains("/usb") {
        "usb"
    } else if name.starts_with("sr") {
        "optical"
    } else if name.starts_with("loop") || name.starts_with("dm-") {
        "virtual"
    } else {
        "sata"
    }
    .into();
    let read = |rel: &str| {
        fs::read_to_string(disk.join(rel))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let vendor = read("device/vendor");
    let model = read("device/model");
    p.model = if vendor.is_empty() || model.starts_with(&vendor) {
        model
    } else {
        format!("{vendor} {model}")
    };
    p.media = Some(if p.bus == "nvme" {
        MediaType::Nvme
    } else if read("queue/rotational") == "1" {
        MediaType::Hdd
    } else {
        MediaType::Ssd
    });
    p
}
