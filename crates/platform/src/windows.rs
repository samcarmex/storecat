//! Windows: volume serials and GUIDs, the storage stack's device descriptor (bus, model,
//! seek penalty), optical drives, and disk health from the Storage Management API.
//!
//! Nothing here needs administrator rights: volume handles are opened with zero access,
//! which is enough for the property queries, and `Get-PhysicalDisk` is readable by users.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use storecat_core::model::{DetectedVolume, Health, MediaType};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::DriveProfile;

const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const STORAGE_DEVICE_PROPERTY: u32 = 0;
const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn root_of(mount: &str) -> String {
    let mut root = mount.to_string();
    if !root.ends_with('\\') {
        root.push('\\');
    }
    root
}

/// (volume serial as 8 hex chars, volume GUID path).
pub(crate) fn volume_ids(mount: &str) -> (Option<String>, Option<String>) {
    let root = wide(&root_of(mount));
    let serial = volume_info(&root).map(|(_, _, s)| s).filter(|&s| s != 0);
    let mut guid_buf = [0u16; 64];
    // SAFETY: `root` is NUL-terminated; the buffer length matches the slice.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(
            root.as_ptr(),
            guid_buf.as_mut_ptr(),
            guid_buf.len() as u32,
        )
    };
    let guid = (ok != 0).then(|| {
        let len = guid_buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&guid_buf[..len])
    });
    (serial.map(|s| format!("{s:08x}")), guid)
}

/// (label, filesystem, serial) of a mounted volume; `None` when nothing is mounted there
/// (for example an empty optical drive).
fn volume_info(root: &[u16]) -> Option<(String, String, u32)> {
    let mut serial: u32 = 0;
    let mut name_buf = [0u16; 261];
    let mut fs_buf = [0u16; 261];
    let mut max_len: u32 = 0;
    let mut flags: u32 = 0;
    // SAFETY: all pointers are valid, correctly sized stack buffers.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            &mut serial,
            &mut max_len,
            &mut flags,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let s = |b: &[u16]| {
        let len = b.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&b[..len])
    };
    Some((s(&name_buf), s(&fs_buf), serial))
}

fn drive_type(path: &str) -> Option<u32> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let root = wide(&format!("{}:\\", bytes[0] as char));
    // SAFETY: NUL-terminated wide string.
    Some(unsafe { GetDriveTypeW(root.as_ptr()) })
}

/// A drive letter that maps a network share.
pub(crate) fn is_remote_drive(path: &str) -> bool {
    drive_type(path) == Some(DRIVE_REMOTE)
}

/// CDs, DVDs and Blu-rays with a disc in the drive. `sysinfo` skips optical drives.
pub(crate) fn optical_volumes(existing: &[DetectedVolume]) -> Vec<DetectedVolume> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let mount = format!("{letter}:\\");
        if existing
            .iter()
            .any(|v| v.mount_path.eq_ignore_ascii_case(&mount))
        {
            continue;
        }
        if drive_type(&mount) != Some(DRIVE_CDROM) {
            continue;
        }
        let root = wide(&mount);
        let Some((label, fs, serial)) = volume_info(&root) else {
            continue; // empty drive
        };
        let mut total: u64 = 0;
        // SAFETY: valid out-pointer; the other two may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root.as_ptr(),
                std::ptr::null_mut(),
                &mut total,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || total == 0 {
            continue;
        }
        let label = if label.trim().is_empty() {
            format!("Disc {letter}:")
        } else {
            label.trim().to_string()
        };
        out.push(DetectedVolume {
            identity: format!("{}:{:08x}:{}", fs.to_lowercase(), serial, total),
            mount_path: mount,
            label,
            fs,
            capacity: total,
            free: 0,
            media_type: MediaType::Optical,
            removable: true,
            guid: None,
            bus: "optical".into(),
            model: String::new(),
            health: Health::Unknown,
        });
    }
    out
}

/// Profiles for a set of mounts: one device query each, plus a single `Get-PhysicalDisk`
/// call for the health of every disk involved.
pub(crate) fn profiles(mounts: &[&str]) -> Vec<DriveProfile> {
    let mut out: Vec<(DriveProfile, Option<u32>)> =
        mounts.iter().map(|m| device_profile(m)).collect();
    if out.iter().any(|(_, n)| n.is_some()) {
        let disks = physical_disks();
        for (p, n) in &mut out {
            let Some(n) = n else { continue };
            if let Some(d) = disks.iter().find(|d| d.number == *n) {
                p.health = d.health;
                if !d.name.trim().is_empty() {
                    p.model = d.name.trim().to_string();
                }
                if p.bus.is_empty() {
                    p.bus = d.bus.to_lowercase();
                }
                if p.media.is_none() {
                    p.media = d.media;
                }
            }
        }
    }
    out.into_iter().map(|(p, _)| p).collect()
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateFileW and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_volume(mount: &str) -> Option<Handle> {
    let path = if mount.len() >= 2 && mount.as_bytes()[1] == b':' {
        format!("\\\\.\\{}", &mount[..2])
    } else {
        // Folder mount point: use the volume GUID path without its trailing slash.
        let (_, guid) = volume_ids(mount);
        guid?.trim_end_matches('\\').to_string()
    };
    let w = wide(&path);
    // SAFETY: zero access rights are enough for property queries and never prompt.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Handle(h))
}

fn ioctl(h: &Handle, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut returned: u32 = 0;
    // SAFETY: buffers are valid for their stated lengths; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            code,
            input.as_ptr() as *const c_void,
            input.len() as u32,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(returned as usize)
}

fn property_query(property: u32) -> [u8; 12] {
    // STORAGE_PROPERTY_QUERY { PropertyId, QueryType = PropertyStandardQuery, pad }
    let mut q = [0u8; 12];
    q[..4].copy_from_slice(&property.to_le_bytes());
    q
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

fn cstr_at(b: &[u8], off: usize) -> String {
    if off == 0 || off >= b.len() {
        return String::new();
    }
    let end = b[off..]
        .iter()
        .position(|&c| c == 0)
        .map(|e| off + e)
        .unwrap_or(b.len());
    String::from_utf8_lossy(&b[off..end]).trim().to_string()
}

fn bus_name(bus: u32) -> &'static str {
    match bus {
        1 => "scsi",
        2 => "atapi",
        3 => "ata",
        4 => "firewire",
        6 => "fibre",
        7 => "usb",
        8 => "raid",
        9 => "iscsi",
        10 => "sas",
        11 => "sata",
        12 => "sd",
        13 => "mmc",
        14 | 15 => "virtual",
        16 => "spaces",
        17 => "nvme",
        19 => "ufs",
        _ => "",
    }
}

/// Bus, model and SSD/HDD from the storage stack, plus the physical disk number.
fn device_profile(mount: &str) -> (DriveProfile, Option<u32>) {
    let mut p = DriveProfile::default();
    if drive_type(mount) == Some(DRIVE_CDROM) {
        p.bus = "optical".into();
        p.media = Some(MediaType::Optical);
        return (p, None);
    }
    let Some(h) = open_volume(mount) else {
        return (p, None);
    };
    let mut buf = [0u8; 1024];
    if let Some(n) = ioctl(
        &h,
        IOCTL_STORAGE_QUERY_PROPERTY,
        &property_query(STORAGE_DEVICE_PROPERTY),
        &mut buf,
    ) {
        let b = &buf[..n];
        // STORAGE_DEVICE_DESCRIPTOR: VendorIdOffset @12, ProductIdOffset @16, BusType @28.
        let vendor = cstr_at(b, u32_at(b, 12) as usize);
        let product = cstr_at(b, u32_at(b, 16) as usize);
        p.model = match (vendor.is_empty(), product.is_empty()) {
            (false, false) if !product.starts_with(&vendor) => format!("{vendor} {product}"),
            (_, false) => product,
            _ => vendor,
        };
        p.bus = bus_name(u32_at(b, 28)).to_string();
        if p.bus == "nvme" {
            p.media = Some(MediaType::Nvme);
        }
    }
    if p.media.is_none() {
        let mut sp = [0u8; 12];
        if ioctl(
            &h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &property_query(STORAGE_DEVICE_SEEK_PENALTY_PROPERTY),
            &mut sp,
        )
        .is_some_and(|n| n >= 9)
        {
            // DEVICE_SEEK_PENALTY_DESCRIPTOR { Version, Size, IncursSeekPenalty: BOOLEAN }
            p.media = Some(if sp[8] != 0 {
                MediaType::Hdd
            } else {
                MediaType::Ssd
            });
        }
    }
    let mut num = [0u8; 12];
    let disk = ioctl(&h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut num)
        .filter(|&n| n >= 8)
        .map(|_| u32_at(&num, 4));
    (p, disk)
}

struct PhysicalDisk {
    number: u32,
    name: String,
    bus: String,
    media: Option<MediaType>,
    health: Health,
}

/// `Get-PhysicalDisk` through PowerShell: the OS's own S.M.A.R.T. summary per disk.
/// Runs hidden, with a timeout, at most once per new drive (results are cached).
fn physical_disks() -> Vec<PhysicalDisk> {
    let script = "Get-PhysicalDisk | Select-Object DeviceId,FriendlyName,BusType,MediaType,HealthStatus | ConvertTo-Json -Compress";
    let Ok(mut child) = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    else {
        return Vec::new();
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                log::warn!("Get-PhysicalDisk timed out");
                return Vec::new();
            }
        }
    }
    let Ok(out) = child.wait_with_output() else {
        return Vec::new();
    };
    parse_physical_disks(&String::from_utf8_lossy(&out.stdout))
}

fn parse_physical_disks(json: &str) -> Vec<PhysicalDisk> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return Vec::new();
    };
    let items = match v {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    let text = |o: &serde_json::Value, k: &str| match &o[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    items
        .iter()
        .filter_map(|o| {
            let number = text(o, "DeviceId").parse().ok()?;
            let health = match &o["HealthStatus"] {
                // Older PowerShell emits the raw CIM value.
                serde_json::Value::Number(n) => match n.as_u64() {
                    Some(0) => Health::Healthy,
                    Some(1) => Health::Warning,
                    Some(2) => Health::Unhealthy,
                    _ => Health::Unknown,
                },
                serde_json::Value::String(s) => Health::parse(s),
                _ => Health::Unknown,
            };
            // "HDD", "SSD", "SCM" or "Unspecified" (older PowerShell: 3, 4, 5 or 0).
            let media = match text(o, "MediaType").as_str() {
                "HDD" | "3" => Some(MediaType::Hdd),
                "SSD" | "SCM" | "4" | "5" => Some(MediaType::Ssd),
                _ => None,
            };
            Some(PhysicalDisk {
                number,
                name: text(o, "FriendlyName"),
                bus: text(o, "BusType"),
                media,
                health,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_array_output() {
        let one = r#"{"DeviceId":"0","FriendlyName":"SAMSUNG MZVL21T0HCLR","BusType":"NVMe","HealthStatus":"Healthy"}"#;
        let d = parse_physical_disks(one);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].health, Health::Healthy);
        let many = r#"[{"DeviceId":"1","FriendlyName":"WD","BusType":"USB","HealthStatus":1},{"DeviceId":"2","HealthStatus":"Unhealthy"}]"#;
        let d = parse_physical_disks(many);
        assert_eq!(d[0].health, Health::Warning);
        assert_eq!(d[1].health, Health::Unhealthy);
    }

    #[test]
    fn reads_media_type_when_windows_knows_it() {
        let json = r#"[{"DeviceId":"1","FriendlyName":"Seagate Expansion HDD","BusType":"USB","MediaType":"Unspecified","HealthStatus":"Healthy"},{"DeviceId":"0","MediaType":"SSD"},{"DeviceId":"2","MediaType":3}]"#;
        let d = parse_physical_disks(json);
        assert_eq!(d[0].media, None);
        assert_eq!(d[1].media, Some(MediaType::Ssd));
        assert_eq!(d[2].media, Some(MediaType::Hdd));
    }

    #[test]
    fn system_drive_has_a_profile() {
        let p = profiles(&["C:\\"]);
        assert_eq!(p.len(), 1);
        // Bus type is always reported. SSD/HDD (seek penalty) is not: virtual disks such as
        // the ones on CI runners leave it unanswered, so `media` may be None there.
        assert!(!p[0].bus.is_empty(), "{:?}", p[0]);
    }
}
