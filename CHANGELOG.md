# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-11-23

### Added

- `-force` re-encodes even when the payload already matches the target format,
  which is the only way to bring a library to one bitrate. The default stays a
  byte-exact copy and `-tag` still remuxes instead of re-encoding.
- `install.sh` resolves the right release for the host, verifies the archive
  against the published checksums, and installs to `~/.local/bin`.
- The crate is published on crates.io.

## [0.4.0] - 2026-11-23

### Added

- `-info` reports the payload container, duration, tags and cover without
  writing anything.
- Releases carry `checksums.txt` for every archive.
- The README gained a recipes section and Mermaid diagrams for the architecture,
  the copy-versus-encode decision, and one conversion.

### Fixed

- Directory traversal recursed forever through a symlink pointing at an
  ancestor. Symlinked files are still converted; symlinked directories are
  skipped.

## [0.3.0] - 2026-11-23

### Added

- `-tag` attaches tags and cover art by remuxing with stream copy, so the audio
  stream is untouched while the container gains a real tag block.
- Cover art is embedded for `m4a` and `mp4` as well as `mp3` and `flac`.

### Fixed

- Two conversions of equal-length covers staged onto the same temporary file and
  deleted it under each other. The ffmpeg argument builder now receives the path
  instead of reconstructing it.
- Every worker staged onto the same temporary path because the name was derived
  from the process id and the extension only.
- ffprobe failure silently changed the output (16-bit WAV, no sample-rate
  clamp); it now warns.

## [0.2.0] - 2026-11-23

### Added

- First Rust release. Container header parsing, AES-128-ECB key unwrap, the
  container key schedule, and SIMD payload decryption (NEON on aarch64, AVX2 on
  x86-64, scalar elsewhere).
- Formats: `original` (byte-exact), `mp3`, `flac`, `wav`, `m4a`, `mp4`.
- Parallel workers, idempotent skipping, JSON manifest, metadata and cover
  propagation.
- CI on Linux, macOS and Windows; tagged releases build four binaries.
