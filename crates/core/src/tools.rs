//! Optional sidecar binaries. FFmpeg is GPL, 100+ MB and not something to bundle, so it is
//! found on the PATH or downloaded on request into the data directory.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::model::FfmpegStatus;
use crate::paths::DataDirs;

#[derive(Debug, Clone)]
pub struct FfmpegPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn find_ffmpeg(dirs: &DataDirs) -> Option<FfmpegPaths> {
    let local = dirs.tools_dir().join("ffmpeg");
    let candidate = FfmpegPaths {
        ffmpeg: local.join(exe("ffmpeg")),
        ffprobe: local.join(exe("ffprobe")),
    };
    if candidate.ffmpeg.is_file() && candidate.ffprobe.is_file() {
        return Some(candidate);
    }
    let on_path = |name: &str| -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|p| p.join(exe(name)))
            .find(|p| p.is_file())
    };
    match (on_path("ffmpeg"), on_path("ffprobe")) {
        (Some(ffmpeg), Some(ffprobe)) => Some(FfmpegPaths { ffmpeg, ffprobe }),
        _ => None,
    }
}

pub fn ffmpeg_status(dirs: &DataDirs) -> FfmpegStatus {
    match find_ffmpeg(dirs) {
        Some(p) => FfmpegStatus {
            available: true,
            version: ffmpeg_version(&p.ffmpeg),
            ffmpeg: Some(p.ffmpeg.to_string_lossy().into_owned()),
            ffprobe: Some(p.ffprobe.to_string_lossy().into_owned()),
        },
        None => FfmpegStatus::default(),
    }
}

fn ffmpeg_version(ffmpeg: &Path) -> Option<String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-version");
    hide_console(&mut cmd);
    let out = cmd.output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .next()
        .and_then(|l| l.strip_prefix("ffmpeg version "))
        .map(|v| v.split_whitespace().next().unwrap_or(v).to_string())
}

/// Where the Windows build comes from. A GPL build of FFmpeg, matching our licence.
pub const FFMPEG_WIN64_URL: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip";

/// Download FFmpeg into `tools/ffmpeg`. `progress(downloaded, total)` is called as bytes
/// arrive. Windows only for now; other platforms get a pointer to their package manager.
pub fn download_ffmpeg(
    dirs: &DataDirs,
    progress: &(dyn Fn(u64, Option<u64>) + Sync),
) -> Result<FfmpegPaths> {
    if !cfg!(windows) {
        return Err(Error::Other(
            "Automatic FFmpeg download is Windows-only for now. Install it with your package \
             manager (brew install ffmpeg / apt install ffmpeg) and Storecat will find it."
                .into(),
        ));
    }
    let dest_dir = dirs.tools_dir().join("ffmpeg");
    std::fs::create_dir_all(&dest_dir)?;
    let zip_path = dirs.tools_dir().join("ffmpeg-download.zip");

    let resp = ureq::get(FFMPEG_WIN64_URL)
        .call()
        .map_err(|e| Error::Other(format!("download failed: {e}")))?;
    let total: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = resp.into_body().into_reader();
    let mut file = std::fs::File::create(&zip_path)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut downloaded = 0u64;
    let mut last_report = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Other(format!("download interrupted: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        if downloaded - last_report > 1024 * 1024 {
            last_report = downloaded;
            progress(downloaded, total);
        }
    }
    file.flush()?;
    drop(file);
    progress(downloaded, total);

    // Pull just the two binaries out of the archive.
    let f = std::fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f))
        .map_err(|e| Error::Other(format!("bad archive: {e}")))?;
    let mut found = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| Error::Other(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        let target = if name.ends_with("/bin/ffmpeg.exe") {
            "ffmpeg.exe"
        } else if name.ends_with("/bin/ffprobe.exe") {
            "ffprobe.exe"
        } else {
            continue;
        };
        let mut out = std::fs::File::create(dest_dir.join(target))?;
        std::io::copy(&mut entry, &mut out)?;
        found += 1;
    }
    let _ = std::fs::remove_file(&zip_path);
    if found < 2 {
        return Err(Error::Other(
            "archive did not contain ffmpeg.exe and ffprobe.exe".into(),
        ));
    }
    find_ffmpeg(dirs).ok_or_else(|| Error::Other("ffmpeg not runnable after download".into()))
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}
