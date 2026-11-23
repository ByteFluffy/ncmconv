//! ffmpeg and ffprobe invocation. No encoder is implemented here.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ncm::Meta;

/// Lossy encoders reject high sample rates; the container often carries
/// upsampled 192 kHz material, so the cap removes weight, not information.
pub const LOSSY_SAMPLE_RATE_CAP: u32 = 48_000;

#[derive(Debug, Clone, Default)]
pub struct AudioInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub bit_rate: u64,
}

pub struct Request<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub format: &'a str,
    pub bitrate: &'a str,
    pub info: &'a AudioInfo,
    pub meta: Option<&'a Meta>,
    pub cover: &'a [u8],
}

impl Request<'_> {
    fn embeds_cover(&self) -> bool {
        !self.cover.is_empty() && (self.format == "mp3" || self.format == "flac")
    }
}

/// Naming a cover file after its size alone made concurrent conversions of
/// covers with equal length share one path, and the first finished worker
/// deleted the file the others were still reading.
static COVER_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn cover_temp_path(cover: &[u8]) -> PathBuf {
    let seq = COVER_SEQ.fetch_add(1, Ordering::Relaxed);
    let ext = if cover.starts_with(b"\x89PNG") {
        "png"
    } else {
        "jpg"
    };
    std::env::temp_dir().join(format!("ncmconv-cover-{}-{seq}.{ext}", std::process::id()))
}

/// Builds the argument vector. Every value is a separate argv entry, so no shell
/// quoting is involved and titles may contain anything.
pub fn ffmpeg_args(req: &Request, cover_path: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-i"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push(req.input.to_string_lossy().into_owned());

    let cover = cover_path.filter(|_| req.embeds_cover());
    if let Some(path) = &cover {
        args.push("-i".into());
        args.push(path.to_string_lossy().into_owned());
    }

    args.push("-map".into());
    args.push("0:a:0".into());
    if cover.is_some() {
        for extra in [
            "-map",
            "1:v:0",
            "-c:v",
            "copy",
            "-disposition:v:0",
            "attached_pic",
        ] {
            args.push(extra.into());
        }
    }

    match req.format {
        "mp3" => {
            args.push("-c:a".into());
            args.push("libmp3lame".into());
            args.push("-b:a".into());
            args.push(req.bitrate.into());
            if req.info.sample_rate > LOSSY_SAMPLE_RATE_CAP {
                args.push("-ar".into());
                args.push(LOSSY_SAMPLE_RATE_CAP.to_string());
            }
            args.push("-id3v2_version".into());
            args.push("3".into());
        }
        "m4a" | "mp4" => {
            args.push("-c:a".into());
            args.push("aac".into());
            args.push("-b:a".into());
            args.push(req.bitrate.into());
            if req.info.sample_rate > LOSSY_SAMPLE_RATE_CAP {
                args.push("-ar".into());
                args.push(LOSSY_SAMPLE_RATE_CAP.to_string());
            }
        }
        "wav" => {
            args.push("-c:a".into());
            args.push(
                if req.info.bits > 16 {
                    "pcm_s24le"
                } else {
                    "pcm_s16le"
                }
                .into(),
            );
        }
        "flac" => {
            args.push("-c:a".into());
            args.push("flac".into());
        }
        _ => {}
    }

    if let Some(meta) = req.meta {
        for (key, value) in [
            ("title", meta.name.as_str()),
            ("artist", meta.artist.as_str()),
            ("album", meta.album.as_str()),
        ] {
            if !value.is_empty() {
                args.push("-metadata".into());
                args.push(format!("{key}={value}"));
            }
        }
    }

    args.push(req.output.to_string_lossy().into_owned());
    args
}

/// Reads stream facts so the output keeps the source depth and so pointless
/// re-encodes can be reported.
pub fn probe(ffprobe: &str, path: &Path) -> Result<AudioInfo, String> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,bits_per_raw_sample,bits_per_sample,bit_rate",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe output: {e}"))?;
    let stream = parsed
        .get("streams")
        .and_then(|s| s.get(0))
        .ok_or_else(|| "ffprobe: no audio stream".to_string())?;

    // ffprobe emits these either as numbers or as strings depending on the
    // container, so both shapes have to be accepted.
    let number = |key: &str| -> u64 {
        match stream.get(key) {
            Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or_default(),
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or_default(),
            _ => 0,
        }
    };
    let bits = {
        let raw = number("bits_per_raw_sample");
        if raw > 0 {
            raw
        } else {
            number("bits_per_sample")
        }
    };

    Ok(AudioInfo {
        codec: stream
            .get("codec_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        sample_rate: number("sample_rate") as u32,
        bits: bits as u32,
        channels: number("channels") as u32,
        bit_rate: number("bit_rate"),
    })
}

pub fn run(ffmpeg: &str, req: &Request) -> Result<(), String> {
    let cover_path = if req.embeds_cover() {
        let path = cover_temp_path(req.cover);
        std::fs::write(&path, req.cover).map_err(|e| format!("write cover: {e}"))?;
        Some(path)
    } else {
        None
    };

    let result = Command::new(ffmpeg)
        .args(ffmpeg_args(req, cover_path.as_deref()))
        .output()
        .map_err(|e| format!("ffmpeg: {e}"));

    if let Some(path) = cover_path {
        let _ = std::fs::remove_file(path);
    }

    let output = result?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(800)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(format!("ffmpeg: {}", tail.trim()));
    }
    Ok(())
}
