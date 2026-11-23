//! Command line surface, work queue and manifest.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;

use crate::ncm::Decoder;
use crate::transcode::{self, AudioInfo, Request};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const VALID_FORMATS: [&str; 6] = ["original", "mp3", "flac", "wav", "m4a", "mp4"];

fn extension_for(format: &str) -> &str {
    match format {
        "mp3" => "mp3",
        "flac" => "flac",
        "wav" => "wav",
        "m4a" => "m4a",
        "mp4" => "mp4",
        "ogg" => "ogg",
        _ => "bin",
    }
}

#[derive(Debug)]
pub struct Options {
    pub out: Option<PathBuf>,
    pub format: String,
    pub bitrate: String,
    pub recursive: bool,
    pub jobs: usize,
    pub overwrite: bool,
    pub manifest: Option<PathBuf>,
    /// Flags the user actually set, so behaviour that ignores one can say so.
    explicit: HashSet<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            out: None,
            format: "original".into(),
            bitrate: "320k".into(),
            recursive: false,
            jobs: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            overwrite: false,
            manifest: None,
            explicit: HashSet::new(),
        }
    }
}

struct Tools {
    ffmpeg: String,
    ffprobe: String,
}

#[derive(Debug, Clone, Default)]
struct Entry {
    source: String,
    output: String,
    from: String,
    to: String,
    size: u64,
    duration: u64,
    title: String,
    artist: String,
    album: String,
    bit_rate: u64,
    skipped: bool,
    error: Option<String>,
}

impl Entry {
    fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("source".into(), json!(self.source));
        map.insert("output".into(), json!(self.output));
        map.insert("source_format".into(), json!(self.from));
        map.insert("output_format".into(), json!(self.to));
        map.insert("size_bytes".into(), json!(self.size));
        map.insert("duration_seconds".into(), json!(self.duration));
        if !self.title.is_empty() {
            map.insert("title".into(), json!(self.title));
        }
        if !self.artist.is_empty() {
            map.insert("artist".into(), json!(self.artist));
        }
        if !self.album.is_empty() {
            map.insert("album".into(), json!(self.album));
        }
        if self.bit_rate > 0 {
            map.insert("bit_rate".into(), json!(self.bit_rate));
        }
        if self.skipped {
            map.insert("skipped".into(), json!(true));
        }
        if let Some(error) = &self.error {
            map.insert("error".into(), json!(error));
        }
        serde_json::Value::Object(map)
    }
}

/// Entry point. Returns the process exit code.
pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return 0;
    }
    if argv.iter().any(|a| a == "-version" || a == "--version") {
        println!("ncmconv {}", VERSION);
        return 0;
    }

    let (options, inputs) = match parse_args(&argv) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}");
            print_usage();
            return 2;
        }
    };
    if inputs.is_empty() {
        eprintln!("error: no input given");
        print_usage();
        return 2;
    }

    let files = match collect(&inputs, options.recursive) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    if files.is_empty() {
        eprintln!("error: no .ncm files found");
        return 2;
    }

    let tools = if options.format == "original" {
        None
    } else {
        match (which("ffmpeg"), which("ffprobe")) {
            (Some(ffmpeg), Some(ffprobe)) => Some(Tools { ffmpeg, ffprobe }),
            _ => {
                eprintln!(
                    "error: ffmpeg and ffprobe are required to change format (brew install ffmpeg)"
                );
                return 2;
            }
        }
    };

    let mut entries = run_queue(&files, &options, tools.as_ref());
    match report(&mut entries, &options) {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run_queue(files: &[PathBuf], options: &Options, tools: Option<&Tools>) -> Vec<Entry> {
    let queue = Mutex::new(VecDeque::from_iter(files.iter().cloned().enumerate()));
    let slots = Mutex::new(vec![Entry::default(); files.len()]);
    let claimed = Mutex::new(HashSet::new());
    let jobs = options.jobs.clamp(1, files.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let next = queue.lock().unwrap().pop_front();
                let Some((index, path)) = next else { break };
                let entry = process(&path, options, tools, &claimed);
                let mut slots = slots.lock().unwrap();
                print_entry(&entry);
                slots[index] = entry;
            });
        }
    });

    slots.into_inner().unwrap()
}

fn process(
    source: &Path,
    options: &Options,
    tools: Option<&Tools>,
    claimed: &Mutex<HashSet<PathBuf>>,
) -> Entry {
    let mut entry = Entry {
        source: source.display().to_string(),
        ..Default::default()
    };

    let mut decoder = match Decoder::open(source) {
        Ok(decoder) => decoder,
        Err(error) => {
            entry.error = Some(error.to_string());
            return entry;
        }
    };
    let from = match decoder.format() {
        Ok(format) => format.to_string(),
        Err(error) => {
            entry.error = Some(error.to_string());
            return entry;
        }
    };
    entry.from = from.clone();

    let meta = decoder.meta.clone();
    if let Some(meta) = &meta {
        entry.duration = meta.duration_seconds();
        entry.title = meta.name.clone();
        entry.artist = meta.artist.clone();
        entry.album = meta.album.clone();
    }

    let target = if options.format == "original" {
        from.clone()
    } else {
        options.format.clone()
    };
    entry.to = target.clone();

    let out_dir = options
        .out
        .clone()
        .unwrap_or_else(|| source.parent().unwrap_or(Path::new(".")).to_path_buf());
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let output = claim(
        &out_dir.join(format!("{stem}.{}", extension_for(&target))),
        claimed,
    );
    entry.output = output.display().to_string();

    if !options.overwrite && output.exists() {
        entry.skipped = true;
        return entry;
    }
    if let Err(error) = std::fs::create_dir_all(&out_dir) {
        entry.error = Some(error.to_string());
        return entry;
    }

    // Same container as the payload: stream it out untouched, never touching an
    // encoder. This is the lossless path.
    if target == from {
        if options.explicit.contains("b") {
            eprintln!(
                "[note] {}: payload is already {target}, copied bit-exactly; -b ignored",
                file_name(source)
            );
        }
        match dump(&mut decoder, &output) {
            Ok(size) => entry.size = size,
            Err(error) => entry.error = Some(error.to_string()),
        }
        return entry;
    }

    let tools = match tools {
        Some(tools) => tools,
        None => {
            entry.error = Some("ffmpeg is required to change format".into());
            return entry;
        }
    };
    let staged = match stage(&mut decoder, &out_dir, &from) {
        Ok(path) => path,
        Err(error) => {
            entry.error = Some(error.to_string());
            return entry;
        }
    };
    let info = transcode::probe(&tools.ffprobe, &staged).unwrap_or_default();
    entry.bit_rate = info.bit_rate;
    if let Some(warning) = warn_reencode(&from, &target, &info) {
        eprintln!("[warn] {}: {warning}", file_name(source));
    }

    let request = Request {
        input: &staged,
        output: &output,
        format: &target,
        bitrate: &options.bitrate,
        info: &info,
        meta: meta.as_ref(),
        cover: &decoder.cover,
    };
    let result = transcode::run(&tools.ffmpeg, &request);
    let _ = std::fs::remove_file(&staged);

    match result {
        Ok(()) => entry.size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0),
        Err(error) => entry.error = Some(error),
    }
    entry
}

/// Two conversions cannot improve on their source; say so.
fn warn_reencode(from: &str, to: &str, info: &AudioInfo) -> Option<String> {
    let lossy_in = from == "mp3" || from == "ogg";
    let lossy_out = matches!(to, "mp3" | "m4a" | "mp4");
    if !lossy_in || !lossy_out {
        return None;
    }
    if info.bit_rate > 0 {
        Some(format!(
            "payload is already lossy ({from} {} kbps); {to} cannot restore lost detail",
            info.bit_rate / 1000
        ))
    } else {
        Some(format!(
            "payload is already lossy ({from}); {to} cannot restore lost detail"
        ))
    }
}

fn dump(decoder: &mut Decoder, output: &Path) -> std::io::Result<u64> {
    let mut file = std::fs::File::create(output)?;
    match decoder.copy_to(&mut file) {
        Ok(size) => Ok(size),
        Err(error) => {
            let _ = std::fs::remove_file(output);
            Err(error)
        }
    }
}

fn stage(decoder: &mut Decoder, dir: &Path, from: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(format!(
        ".ncmconv-{}.{}",
        std::process::id(),
        extension_for(from)
    ));
    let mut file = std::fs::File::create(&path)?;
    match decoder.copy_to(&mut file) {
        Ok(_) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
}

/// Serialises output paths so parallel workers cannot write the same file.
fn claim(path: &Path, claimed: &Mutex<HashSet<PathBuf>>) -> PathBuf {
    let mut claimed = claimed.lock().unwrap();
    let mut candidate = path.to_path_buf();
    let mut index = 2;
    while claimed.contains(&candidate) {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        candidate = parent.join(format!("{stem} ({index}).{ext}"));
        index += 1;
    }
    claimed.insert(candidate.clone());
    candidate
}

fn collect(inputs: &[String], recursive: bool) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for input in inputs {
        let path = PathBuf::from(input);
        let metadata = std::fs::metadata(&path).map_err(|e| format!("{input}: {e}"))?;
        if metadata.is_file() {
            files.push(path);
            continue;
        }
        walk(&path, recursive, &mut files)?;
    }
    Ok(files)
}

fn walk(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                walk(&path, recursive, files)?;
            }
        } else if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("ncm"))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn report(entries: &mut [Entry], options: &Options) -> Result<(), ()> {
    let mut converted = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut total = 0u64;
    for entry in entries.iter() {
        match (&entry.error, entry.skipped) {
            (Some(_), _) => failed += 1,
            (None, true) => skipped += 1,
            _ => {
                converted += 1;
                total += entry.size;
            }
        }
    }

    if let Some(path) = &options.manifest {
        entries.sort_by(|a, b| a.source.cmp(&b.source));
        let rows: Vec<serde_json::Value> = entries.iter().map(Entry::to_json).collect();
        let body = serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into());
        if let Err(error) = std::fs::write(path, body + "\n") {
            eprintln!("error: cannot write manifest: {error}");
        }
    }

    println!(
        "\n{converted} converted ({}), {skipped} skipped, {failed} failed",
        human_bytes(total)
    );
    if failed > 0 {
        Err(())
    } else {
        Ok(())
    }
}

fn print_entry(entry: &Entry) {
    match (&entry.error, entry.skipped) {
        (Some(error), _) => eprintln!("[fail] {}: {error}", file_name(Path::new(&entry.source))),
        (None, true) => println!("[skip] {} (exists)", file_name(Path::new(&entry.output))),
        _ => println!(
            "[ ok ] {} -> {} ({}, {})",
            file_name(Path::new(&entry.source)),
            file_name(Path::new(&entry.output)),
            entry.to,
            human_bytes(entry.size)
        ),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

fn parse_args(argv: &[String]) -> Result<(Options, Vec<String>), String> {
    let mut options = Options::default();
    let mut inputs = Vec::new();
    let mut index = 0;

    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if !arg.starts_with('-') || arg == "-" {
            inputs.push(arg.clone());
            continue;
        }
        let name = arg.trim_start_matches('-');
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (name, None),
        };

        let take_value = |index: &mut usize| -> Result<String, String> {
            if let Some(value) = inline.clone() {
                return Ok(value);
            }
            let value = argv
                .get(*index)
                .ok_or_else(|| format!("flag -{name} needs a value"))?;
            *index += 1;
            Ok(value.clone())
        };

        match name {
            "o" | "out" => options.out = Some(PathBuf::from(take_value(&mut index)?)),
            "format" => options.format = take_value(&mut index)?.to_lowercase(),
            "b" | "bitrate" => {
                options.bitrate = take_value(&mut index)?;
                options.explicit.insert("b".into());
            }
            "manifest" => options.manifest = Some(PathBuf::from(take_value(&mut index)?)),
            "j" | "jobs" => {
                let value = take_value(&mut index)?;
                options.jobs = value
                    .parse()
                    .map_err(|_| format!("-j expects a number, got {value}"))?;
            }
            "r" | "recursive" => options.recursive = true,
            "overwrite" => options.overwrite = true,
            other => return Err(format!("unknown flag -{other}")),
        }
    }

    if !VALID_FORMATS.contains(&options.format.as_str()) {
        return Err(format!(
            "unsupported format {:?} (choose from {})",
            options.format,
            VALID_FORMATS.join(", ")
        ));
    }
    Ok((options, inputs))
}

fn print_usage() {
    eprintln!(
        "ncmconv {VERSION} - decrypt NetEase .ncm files and convert them to open formats

Usage:
  ncmconv [options] <file-or-dir>...

Options:
  -o DIR          output directory (default: next to each source file)
  -format FMT     original (default) | mp3 | flac | wav | m4a | mp4
  -b RATE         bitrate for lossy formats (default 320k)
  -r              recurse into directories
  -j N            parallel workers (default: CPU count)
  -overwrite      replace existing output files
  -manifest PATH  also write a JSON manifest
  -version        print version

The default -format original is bit-exact: .ncm only wraps the original MP3 or
FLAC, so decrypting never re-encodes. Any other format goes through ffmpeg and
is lossy for mp3/m4a/mp4."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ncm::fixture::{self, Meta0};
    use std::process::Command;

    fn has_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ncmconv-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A payload with the right magic and arbitrary content. Tests that only
    /// exercise the container path need no real audio, so they stay runnable on
    /// machines without ffmpeg.
    fn fake_payload(magic: &[u8]) -> Vec<u8> {
        let mut payload = magic.to_vec();
        payload.extend((0..4096).map(|i| (i % 251) as u8));
        payload
    }

    /// Synthesises a real 2-second file so the encoder paths see genuine media.
    fn synth(dir: &Path, ext: &str) -> Vec<u8> {
        let path = dir.join(format!("fixture.{ext}"));
        let codec = if ext == "mp3" {
            vec!["-c:a", "libmp3lame", "-b:a", "192k"]
        } else {
            vec!["-c:a", "flac"]
        };
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-ac",
                "2",
                "-ar",
                "44100",
            ])
            .args(codec)
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read(&path).unwrap()
    }

    /// A 192 kHz / 24-bit FLAC. NetEase ships hi-res payloads, and the two
    /// branches that only such a payload reaches are the depth-preserving WAV
    /// encode and the sample-rate clamp for lossy targets.
    fn synth_hi_res(dir: &Path) -> Vec<u8> {
        let path = dir.join("hires.flac");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-ac",
                "2",
                "-ar",
                "192000",
                "-c:a",
                "flac",
                "-sample_fmt",
                "s32",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read(&path).unwrap()
    }

    fn cover_png(dir: &Path) -> Vec<u8> {
        let path = dir.join("cover.png");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x64:d=1",
                "-frames:v",
                "1",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read(&path).unwrap()
    }

    fn options(format: &str, out: &Path) -> Options {
        Options {
            out: Some(out.to_path_buf()),
            format: format.into(),
            ..Default::default()
        }
    }

    fn tools() -> Tools {
        Tools {
            ffmpeg: which("ffmpeg").unwrap(),
            ffprobe: which("ffprobe").unwrap(),
        }
    }

    fn write_ncm(dir: &Path, name: &str, payload: &[u8], cover: &[u8], pad: usize) -> PathBuf {
        let meta = Meta0 {
            name: "Tone",
            album: "Fixtures",
            artist: "Tester",
            duration_ms: 2000,
        };
        let bytes = fixture::build(payload, Some(meta), cover, pad);
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn fresh_claims() -> Mutex<HashSet<PathBuf>> {
        Mutex::new(HashSet::new())
    }

    #[test]
    fn original_export_is_byte_exact() {
        let dir = temp_dir("exact");
        let flac = fake_payload(b"fLaC\x00\x00\x00\x22");
        let ncm = write_ncm(&dir, "a.ncm", &flac, &[], 0);
        let out = temp_dir("exact-out");

        let entry = process(&ncm, &options("original", &out), None, &fresh_claims());
        assert!(entry.error.is_none(), "{:?}", entry.error);
        let written = std::fs::read(&entry.output).unwrap();
        assert_eq!(written, flac);
        assert_eq!(entry.duration, 2);
    }

    #[test]
    fn second_run_skips_unless_overwrite() {
        let dir = temp_dir("skip");
        let flac = fake_payload(b"ID3\x03\x00\x00\x00\x00\x00\x00");
        let ncm = write_ncm(&dir, "a.ncm", &flac, &[], 0);
        let out = temp_dir("skip-out");

        let first = process(&ncm, &options("original", &out), None, &fresh_claims());
        assert!(!first.skipped && first.error.is_none());
        let second = process(&ncm, &options("original", &out), None, &fresh_claims());
        assert!(second.skipped);

        let mut overwrite = options("original", &out);
        overwrite.overwrite = true;
        let third = process(&ncm, &overwrite, None, &fresh_claims());
        assert!(!third.skipped && third.error.is_none());
    }

    #[test]
    fn cover_padding_is_skipped() {
        let dir = temp_dir("pad");
        let flac = fake_payload(b"fLaC\x00\x00\x00\x22");
        // Cover frame declares more bytes than the image it carries.
        let ncm = write_ncm(&dir, "a.ncm", &flac, &[], 37);
        let out = temp_dir("pad-out");
        let entry = process(&ncm, &options("original", &out), None, &fresh_claims());
        assert!(entry.error.is_none(), "{:?}", entry.error);
        assert_eq!(std::fs::read(&entry.output).unwrap(), flac);
    }

    #[test]
    fn transcode_matrix() {
        if !has_ffmpeg() {
            eprintln!("skipping: ffmpeg not installed");
            return;
        }
        let dir = temp_dir("matrix");
        let flac = synth(&dir, "flac");
        let mp3 = synth(&dir, "mp3");
        let png = cover_png(&dir);
        let flac_ncm = write_ncm(&dir, "a.ncm", &flac, &png, 0);
        let mp3_ncm = write_ncm(&dir, "b.ncm", &mp3, &png, 0);
        let tools = tools();

        let cases = [
            ("original", &flac_ncm, "flac"),
            ("flac", &mp3_ncm, "flac"),
            ("mp3", &flac_ncm, "mp3"),
            ("wav", &flac_ncm, "pcm_s16le"),
            ("m4a", &flac_ncm, "aac"),
            ("mp4", &flac_ncm, "aac"),
        ];
        for (format, source, codec) in cases {
            let out = temp_dir(&format!("matrix-{format}"));
            let entry = process(
                source,
                &options(format, &out),
                Some(&tools),
                &fresh_claims(),
            );
            assert!(entry.error.is_none(), "{format}: {:?}", entry.error);
            let info = transcode::probe(&tools.ffprobe, Path::new(&entry.output)).unwrap();
            assert_eq!(info.codec, codec, "{format}");
        }
    }

    #[test]
    fn hi_res_payload_keeps_depth_and_clamps_rate() {
        if !has_ffmpeg() {
            return;
        }
        let dir = temp_dir("hires");
        let payload = synth_hi_res(&dir);
        let ncm = write_ncm(&dir, "h.ncm", &payload, &[], 0);
        let tools = tools();

        let out = temp_dir("hires-wav");
        let entry = process(&ncm, &options("wav", &out), Some(&tools), &fresh_claims());
        assert!(entry.error.is_none(), "{:?}", entry.error);
        let info = transcode::probe(&tools.ffprobe, Path::new(&entry.output)).unwrap();
        assert_eq!(info.codec, "pcm_s24le", "24-bit depth must survive to WAV");
        assert_eq!(info.sample_rate, 192_000, "WAV keeps the source rate");

        for format in ["mp3", "m4a"] {
            let out = temp_dir(&format!("hires-{format}"));
            let entry = process(&ncm, &options(format, &out), Some(&tools), &fresh_claims());
            assert!(entry.error.is_none(), "{format}: {:?}", entry.error);
            let info = transcode::probe(&tools.ffprobe, Path::new(&entry.output)).unwrap();
            assert_eq!(info.sample_rate, 48_000, "{format} clamps the rate");
        }
    }

    #[test]
    fn mp3_target_embeds_cover() {
        if !has_ffmpeg() {
            return;
        }
        let dir = temp_dir("cover");
        let flac = synth(&dir, "flac");
        let png = cover_png(&dir);
        let ncm = write_ncm(&dir, "a.ncm", &flac, &png, 0);
        let out = temp_dir("cover-out");
        let entry = process(&ncm, &options("mp3", &out), Some(&tools()), &fresh_claims());
        assert!(entry.error.is_none(), "{:?}", entry.error);

        let ffprobe = which("ffprobe").unwrap();
        let output = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v",
                "-show_entries",
                "stream_disposition=attached_pic",
                "-of",
                "csv=p=0",
            ])
            .arg(&entry.output)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1");
    }

    #[test]
    fn rejects_foreign_file() {
        let dir = temp_dir("foreign");
        let path = dir.join("fake.ncm");
        std::fs::write(&path, b"not an ncm file at all, really not").unwrap();
        let entry = process(&path, &options("original", &dir), None, &fresh_claims());
        assert!(entry.error.is_some());
    }
}
