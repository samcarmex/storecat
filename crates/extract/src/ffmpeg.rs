//! Optional FFmpeg sidecar: `ffprobe` for stream info on anything, `ffmpeg` for video
//! frames. The binaries are located and downloaded by the core crate; this module only
//! runs them.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::meta::FileMeta;
use crate::thumbs::{encode, Thumb};

const TIMEOUT: Duration = Duration::from_secs(40);

/// Why FFmpeg produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfError {
    /// It ran past the time limit and was stopped: trying the same file again would only
    /// spend that long again.
    TimedOut,
    /// It finished without a usable result.
    Failed,
}

/// Input options for `path`. Some Flash videos (Telestream Wirecast recordings, for one)
/// store each tag's "previous tag size" in the wrong byte order. FFmpeg takes every tag as
/// damaged and reads the whole file looking for a good one, finding none; told to ignore
/// those fields, it reads them in a fraction of a second. They are only a consistency
/// check, so ignoring them costs nothing on well-formed files.
fn input_options(path: &Path) -> &'static [&'static str] {
    let flv = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("flv"));
    if flv {
        &["-flv_ignore_prevtag", "1"]
    } else {
        &[]
    }
}

/// Run `ffprobe` and map the JSON into `FileMeta`.
pub fn probe(ffprobe: &Path, path: &Path) -> Result<FileMeta, FfError> {
    let out = run_with_timeout(
        Command::new(ffprobe)
            .args(["-v", "error"])
            .args(input_options(path))
            .args(["-print_format", "json", "-show_format", "-show_streams"])
            .arg(path),
        TIMEOUT,
    )?;
    let v: serde_json::Value = serde_json::from_slice(&out).map_err(|_| FfError::Failed)?;
    if v.get("format").is_none() && v.get("streams").is_none() {
        return Err(FfError::Failed);
    }
    let mut m = FileMeta::default();
    if let Some(fmt) = v.get("format") {
        m.duration_ms = fmt
            .get("duration")
            .and_then(|d| d.as_str())
            .and_then(|d| d.parse::<f64>().ok())
            .map(|s| (s * 1000.0) as u64);
        m.bitrate_kbps = fmt
            .get("bit_rate")
            .and_then(|b| b.as_str())
            .and_then(|b| b.parse::<u64>().ok())
            .map(|b| (b / 1000) as u32);
        if let Some(tags) = fmt.get("tags").and_then(|t| t.as_object()) {
            for (k, val) in tags {
                let Some(s) = val.as_str() else { continue };
                match k.to_lowercase().as_str() {
                    "title" => m.title = Some(s.to_string()),
                    "artist" => m.artist = Some(s.to_string()),
                    "album" => m.album = Some(s.to_string()),
                    "encoder" => m.software = Some(s.to_string()),
                    "creation_time" => m.doc_created = Some(s.to_string()),
                    _ => {}
                }
            }
        }
    }
    if let Some(streams) = v.get("streams").and_then(|s| s.as_array()) {
        for s in streams {
            match s.get("codec_type").and_then(|t| t.as_str()) {
                Some("video") if m.video_codec.is_none() => {
                    m.video_codec = s
                        .get("codec_name")
                        .and_then(|c| c.as_str())
                        .map(String::from);
                    m.width = s.get("width").and_then(|w| w.as_u64()).map(|w| w as u32);
                    m.height = s.get("height").and_then(|h| h.as_u64()).map(|h| h as u32);
                    m.fps = s
                        .get("avg_frame_rate")
                        .and_then(|f| f.as_str())
                        .and_then(parse_ratio);
                    if let Some(p) = s.get("profile").and_then(|p| p.as_str()) {
                        m.extra.insert("profile".into(), p.to_string());
                    }
                    if let Some(p) = s.get("pix_fmt").and_then(|p| p.as_str()) {
                        m.extra.insert("pixel_format".into(), p.to_string());
                    }
                }
                Some("audio") if m.audio_codec.is_none() => {
                    m.audio_codec = s
                        .get("codec_name")
                        .and_then(|c| c.as_str())
                        .map(String::from);
                    m.sample_rate = s
                        .get("sample_rate")
                        .and_then(|r| r.as_str())
                        .and_then(|r| r.parse().ok());
                    m.channels = s.get("channels").and_then(|c| c.as_u64()).map(|c| c as u8);
                }
                _ => {}
            }
        }
    }
    Ok(m)
}

fn parse_ratio(s: &str) -> Option<f32> {
    let (a, b) = s.split_once('/')?;
    let (a, b): (f32, f32) = (a.parse().ok()?, b.parse().ok()?);
    if b == 0.0 {
        None
    } else {
        Some(a / b)
    }
}

/// Grab one frame at roughly 10% in (or 3 s when the duration is unknown). When the first
/// try fails quickly, the first frame is tried; one that ran out of time is not repeated.
pub fn video_thumbnail(
    ffmpeg: &Path,
    path: &Path,
    duration_ms: Option<u64>,
    max_edge: u32,
) -> Option<Thumb> {
    let seek = duration_ms
        .filter(|d| *d > 2000)
        .map(|d| (d as f64 / 1000.0 * 0.1).min(120.0))
        .unwrap_or(3.0);
    let attempt = |seek_s: f64| -> Result<Vec<u8>, FfError> {
        run_with_timeout(
            Command::new(ffmpeg)
                .args(["-v", "error"])
                .args(input_options(path))
                .args(["-ss", &format!("{seek_s:.2}"), "-i"])
                .arg(path)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    &format!("scale='min({max_edge},iw)':-2"),
                    "-f",
                    "image2pipe",
                    "-vcodec",
                    "mjpeg",
                    "-q:v",
                    "4",
                    "pipe:1",
                ]),
            TIMEOUT,
        )
        .and_then(|b| {
            if b.is_empty() {
                Err(FfError::Failed)
            } else {
                Ok(b)
            }
        })
    };
    let bytes = match attempt(seek) {
        Ok(b) => b,
        Err(FfError::TimedOut) => return None,
        Err(FfError::Failed) => attempt(0.0).ok()?,
    };
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    Some(encode(img, max_edge))
}

/// Run a command, capture stdout, kill it if it exceeds `timeout`.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Vec<u8>, FfError> {
    cmd.stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    hide_console(cmd);
    let mut child = cmd.spawn().map_err(|_| FfError::Failed)?;
    // Package managers (Chocolatey, Scoop) put launchers on PATH that start the real
    // FFmpeg as a child. Killing the launcher alone would leave FFmpeg reading the disk
    // long after Storecat gave up on it, so the whole tree goes in a job that dies with it.
    let job = tree::Tree::adopt(&child);
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Err(FfError::Failed);
    };
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                drop(job);
                let out = reader.join().map_err(|_| FfError::Failed)?;
                return if status.success() {
                    Ok(out)
                } else {
                    Err(FfError::Failed)
                };
            }
            Ok(None) if started.elapsed() > timeout => {
                drop(job); // ends the launched FFmpeg too
                let _ = child.kill();
                let _ = child.wait();
                return Err(FfError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                drop(job);
                let _ = child.kill();
                return Err(FfError::Failed);
            }
        }
    }
}

#[cfg(windows)]
mod tree {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A child process and everything it starts; dropping this ends any still running.
    pub struct Tree(HANDLE);

    impl Tree {
        pub fn adopt(child: &std::process::Child) -> Option<Tree> {
            // SAFETY: plain Win32 calls on a job handle we own and a live child's handle.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let tree = Tree(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                    return None;
                }
                Some(tree)
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod tree {
    /// Elsewhere FFmpeg is run directly, not through a launcher: killing it is enough.
    pub struct Tree;
    impl Tree {
        pub fn adopt(_child: &std::process::Child) -> Option<Tree> {
            Some(Tree)
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {}
    }
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_videos_ignore_tag_sizes() {
        assert_eq!(
            input_options(Path::new(r"D:\VIDEO\SHOW2014_1.FLV")),
            &["-flv_ignore_prevtag", "1"]
        );
        assert!(input_options(Path::new("clip.mp4")).is_empty());
        assert!(input_options(Path::new("flv")).is_empty());
    }

    /// A launcher that starts the real program as a child (as Chocolatey's shims do) is
    /// stopped together with that child when it runs out of time.
    #[cfg(windows)]
    #[test]
    fn timing_out_stops_what_a_launcher_started() {
        let dir = tempfile::tempdir().unwrap();
        let ticks = dir.path().join("ticks.txt");
        let looper = dir.path().join("loop.cmd");
        std::fs::write(
            &looper,
            "@echo off\r\n:a\r\necho x>>\"%~dp0ticks.txt\"\r\nping -n 2 127.0.0.1 >nul\r\ngoto a\r\n",
        )
        .unwrap();
        let mut launcher = Command::new("cmd");
        launcher.args(["/c", "cmd", "/c"]).arg(&looper);
        let got = run_with_timeout(&mut launcher, Duration::from_millis(2500));
        assert_eq!(got, Err(FfError::TimedOut));
        std::thread::sleep(Duration::from_millis(1500));
        let len = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        assert!(len > 0, "the child ran");
        std::thread::sleep(Duration::from_millis(3000));
        let later = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            len, later,
            "the child kept running after its launcher was stopped"
        );
    }
}
