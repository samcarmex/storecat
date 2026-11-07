//! Optional FFmpeg sidecar: `ffprobe` for stream info on anything, `ffmpeg` for video
//! frames. The binaries are located and downloaded by the core crate; this module only
//! runs them.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::meta::FileMeta;
use crate::thumbs::{encode, Thumb};

const TIMEOUT: Duration = Duration::from_secs(40);

/// Run `ffprobe` and map the JSON into `FileMeta`.
pub fn probe(ffprobe: &Path, path: &Path) -> Option<FileMeta> {
    let out = run_with_timeout(
        Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path),
        TIMEOUT,
    )?;
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
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
    Some(m)
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

/// Grab one frame at roughly 10% in (or 3 s when the duration is unknown).
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
    let attempt = |seek_s: f64| -> Option<Vec<u8>> {
        run_with_timeout(
            Command::new(ffmpeg)
                .args(["-v", "error", "-ss", &format!("{seek_s:.2}"), "-i"])
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
        .filter(|b| !b.is_empty())
    };
    let bytes = attempt(seek).or_else(|| attempt(0.0))?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    Some(encode(img, max_edge))
}

/// Run a command, capture stdout, kill it if it exceeds `timeout`.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    hide_console(cmd);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return if status.success() { Some(out) } else { None };
            }
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => return None,
        }
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
