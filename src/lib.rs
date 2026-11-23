//! Decrypt NetEase Cloud Music `.ncm` containers and convert the payload.
//!
//! The container is not an audio format: it wraps the untouched source file
//! (usually MP3 or FLAC). Decryption therefore yields the original bytes, and
//! all format conversion is delegated to ffmpeg.

pub mod cli;
pub mod ncm;
pub mod transcode;
