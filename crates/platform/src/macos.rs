//! macOS: volume UUID, drive profile and S.M.A.R.T. status from `diskutil info`, network
//! mounts from `mount`. `diskutil` is slow-ish (~50 ms), so results are cached per mount.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use storecat_core::model::{Health, MediaType};

use crate::{parse_kv, DriveProfile};

static INFO: Mutex<Option<HashMap<String, HashMap<String, String>>>> = Mutex::new(None);

fn diskutil_info(mount: &str) -> HashMap<String, String> {
    let mut cache = INFO.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(v) = cache.get(mount) {
        return v.clone();
    }
    let out = Command::new("/usr/sbin/diskutil")
        .args(["info", mount])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let kv = parse_kv(&out, ':');
    cache.insert(mount.to_string(), kv.clone());
    kv
}

pub(crate) fn volume_uuid(mount: &str) -> (Option<String>, Option<String>) {
    let info = diskutil_info(mount);
    let uuid = info
        .get("Volume UUID")
        .or_else(|| info.get("Disk / Partition UUID"))
        .cloned();
    (uuid, None)
}

pub(crate) fn is_remote_mount(path: &str) -> bool {
    let out = Command::new("/sbin/mount")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    // "//user@nas/share on /Volumes/share (smbfs, nodev, nosuid, mounted by me)"
    out.lines()
        .filter_map(|l| {
            let (_, rest) = l.split_once(" on ")?;
            let (mnt, opts) = rest.rsplit_once(" (")?;
            let fs = opts.split([',', ')']).next()?.trim();
            Some((mnt.to_string(), fs.to_string()))
        })
        .filter(|(m, _)| path.starts_with(m.as_str()))
        .max_by_key(|(m, _)| m.len())
        .is_some_and(|(_, fs)| matches!(fs.as_str(), "smbfs" | "nfs" | "afpfs" | "webdav"))
}

pub(crate) fn profile(mount: &str) -> DriveProfile {
    let info = diskutil_info(mount);
    let get = |k: &str| info.get(k).cloned().unwrap_or_default();
    let protocol = get("Protocol").to_lowercase();
    let mut p = DriveProfile {
        model: get("Device / Media Name"),
        ..Default::default()
    };
    p.bus = match protocol.as_str() {
        "usb" => "usb",
        "pci-express" | "apple fabric" => "nvme",
        "sata" => "sata",
        "thunderbolt" => "thunderbolt",
        "secure digital" => "sd",
        "disk image" => "virtual",
        _ if get("Optical Drive Type").is_empty() => "",
        _ => "optical",
    }
    .into();
    p.media = if p.bus == "optical" {
        Some(MediaType::Optical)
    } else if p.bus == "nvme" {
        Some(MediaType::Nvme)
    } else {
        match get("Solid State").as_str() {
            "Yes" => Some(MediaType::Ssd),
            "No" => Some(MediaType::Hdd),
            _ => None,
        }
    };
    p.health = match get("SMART Status").as_str() {
        "Verified" => Health::Healthy,
        "Failing" => Health::Unhealthy,
        _ => Health::Unknown,
    };
    p
}
