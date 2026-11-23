//! Container parsing and payload decryption.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;
use base64::Engine;

pub const MAGIC: &[u8; 8] = b"CTENFDAM";
const CORE_KEY: [u8; 16] = [
    0x68, 0x7A, 0x48, 0x52, 0x41, 0x6D, 0x73, 0x6F, 0x35, 0x6B, 0x49, 0x6E, 0x62, 0x61, 0x78, 0x57,
];
const META_KEY: [u8; 16] = [
    0x23, 0x31, 0x34, 0x6C, 0x6A, 0x6B, 0x5F, 0x21, 0x5C, 0x5D, 0x26, 0x30, 0x55, 0x3C, 0x27, 0x28,
];
const META_PLAIN_TAG: &[u8] = b"163 key(Don't modify):";
const META_JSON_TAG: &[u8] = b"music:";
const KEYBOX_PREFIX: usize = 17;

/// Payload is processed in whole chunks so the keystream always starts at
/// phase zero; every chunk size is a multiple of the 256-byte period.
const CHUNK: usize = 64 * 1024;

/// Tag data carried by the container. Every field is optional in practice.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub name: String,
    pub album: String,
    pub artist: String,
    /// The container stores milliseconds.
    pub duration_ms: u64,
}

impl Meta {
    pub fn duration_seconds(&self) -> u64 {
        self.duration_ms / 1000
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// An opened container with its header parsed and its payload still unread.
pub struct Decoder {
    reader: BufReader<File>,
    pub meta: Option<Meta>,
    pub cover: Vec<u8>,
    /// The keystream expanded to its period, with 64 trailing bytes so a
    /// 64-byte SIMD read starting anywhere in the period stays in bounds.
    ks: [u8; 320],
}

impl Decoder {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut decoder = Decoder {
            reader: BufReader::with_capacity(CHUNK, file),
            meta: None,
            cover: Vec::new(),
            ks: [0; 320],
        };
        decoder.parse_header()?;
        Ok(decoder)
    }

    fn parse_header(&mut self) -> io::Result<()> {
        let mut head = [0u8; MAGIC.len() + 2];
        self.reader.read_exact(&mut head)?;
        if &head[..MAGIC.len()] != MAGIC {
            return Err(invalid("not an NCM file (bad magic)"));
        }

        let mut key_data = self
            .read_block("stream key")?
            .ok_or_else(|| invalid("missing stream key"))?;
        for byte in key_data.iter_mut() {
            *byte ^= 0x64;
        }
        aes_ecb_decrypt(&CORE_KEY, &mut key_data);
        strip_pkcs7(&mut key_data);
        if key_data.len() <= KEYBOX_PREFIX {
            return Err(invalid("decrypted stream key is too short"));
        }
        self.ks = build_keystream_ext(&build_keybox(&key_data[KEYBOX_PREFIX..]));

        if let Some(meta) = self.read_block("metadata")? {
            self.meta = parse_meta(meta);
        }

        // CRC32 of the whole file, then the cover-frame version.
        skip(&mut self.reader, 5)?;
        let cover_frame_len = read_u32(&mut self.reader)? as u64;
        let image_len = read_u32(&mut self.reader)? as u64;
        if cover_frame_len < image_len {
            return Err(invalid("corrupt cover frame"));
        }
        if image_len > 0 {
            self.cover = vec![0u8; image_len as usize];
            self.reader.read_exact(&mut self.cover)?;
        }
        skip(&mut self.reader, cover_frame_len - image_len)?;
        Ok(())
    }

    fn read_block(&mut self, what: &str) -> io::Result<Option<Vec<u8>>> {
        let n = read_u32(&mut self.reader)? as usize;
        if n == 0 {
            return Ok(None);
        }
        let mut buf = vec![0u8; n];
        self.reader
            .read_exact(&mut buf)
            .map_err(|e| io::Error::new(e.kind(), format!("read {what}: {e}")))?;
        Ok(Some(buf))
    }

    /// Container of the encrypted payload, detected from its first bytes rather
    /// than trusted from the metadata.
    pub fn format(&mut self) -> io::Result<&'static str> {
        let head = self.reader.fill_buf()?;
        if head.len() < 16 {
            return Err(invalid("payload is shorter than 16 bytes"));
        }
        let mut plain = [0u8; 16];
        for (i, out) in plain.iter_mut().enumerate() {
            *out = head[i] ^ self.ks[i];
        }
        detect_format(&plain)
    }

    /// Writes the decrypted payload. Byte-for-byte, so an export in the payload
    /// format is lossless by construction.
    pub fn copy_to<W: Write>(&mut self, out: &mut W) -> io::Result<u64> {
        let mut buf = vec![0u8; CHUNK];
        let mut plain = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            let n = fill(&mut self.reader, &mut buf)?;
            if n == 0 {
                break;
            }
            xor_payload(&mut plain[..n], &buf[..n], &self.ks);
            out.write_all(&plain[..n])?;
            total += n as u64;
            if n < CHUNK {
                break;
            }
        }
        Ok(total)
    }
}

fn detect_format(head: &[u8]) -> io::Result<&'static str> {
    if head.starts_with(b"ID3") {
        Ok("mp3")
    } else if head.starts_with(b"fLaC") {
        Ok("flac")
    } else if head.starts_with(b"OggS") {
        Ok("ogg")
    } else if head.starts_with(b"RIFF") {
        Ok("wav")
    } else if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 {
        // A bare MPEG frame header means MP3 without an ID3 tag.
        Ok("mp3")
    } else {
        Err(invalid("unknown payload format (not mp3/flac/ogg/wav)"))
    }
}

fn parse_meta(mut raw: Vec<u8>) -> Option<Meta> {
    for byte in raw.iter_mut() {
        *byte ^= 0x63;
    }
    let body = raw.strip_prefix(META_PLAIN_TAG)?;
    let encoded: Vec<u8> = body
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut blob = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .ok()?;
    aes_ecb_decrypt(&META_KEY, &mut blob);
    strip_pkcs7(&mut blob);

    let start = find(&blob, META_JSON_TAG)? + META_JSON_TAG.len();
    let mut value: serde_json::Value = serde_json::from_slice(&blob[start..]).ok()?;

    let mut take = |key: &str| -> String {
        value
            .get_mut(key)
            .and_then(|v| v.take().as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let name = take("musicName");
    let album = take("album");
    let duration_ms = value
        .get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();

    let artist = value
        .get("artist")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get(0).and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();

    Some(Meta {
        name,
        album,
        artist,
        duration_ms,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn aes_ecb_decrypt(key: &[u8; 16], data: &mut [u8]) {
    if data.is_empty() {
        return;
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for block in data.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
}

/// Removes the block padding the container applies to its AES-encrypted blobs.
fn strip_pkcs7(data: &mut Vec<u8>) {
    if let Some(&pad) = data.last() {
        let pad = pad as usize;
        if pad <= 16 && pad <= data.len() {
            data.truncate(data.len() - pad);
        }
    }
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn skip<R: Read>(reader: &mut R, n: u64) -> io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    let copied = io::copy(&mut reader.take(n), &mut io::sink())?;
    if copied != n {
        return Err(invalid("unexpected end of file while skipping a frame"));
    }
    Ok(())
}

/// Fills `buf` unless the file ends first.
fn fill<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(read) => n += read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// The container key schedule. This is not RC4: each step takes the current box
/// value as the swap source and mixes the key byte into a running counter.
fn build_keybox(key: &[u8]) -> [u8; 256] {
    let mut boxed: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut last = 0u8;
    let mut offset = 0usize;
    for i in 0..256 {
        let swap = boxed[i];
        let c = swap.wrapping_add(last).wrapping_add(key[offset]);
        offset += 1;
        if offset >= key.len() {
            offset = 0;
        }
        boxed[i] = boxed[c as usize];
        boxed[c as usize] = swap;
        last = c;
    }
    boxed
}

/// One keystream byte. Single definition of the cipher; the hot path only
/// consumes the expanded period built from this.
fn stream_byte(boxed: &[u8; 256], j: u8) -> u8 {
    let k = boxed[j as usize] as usize;
    let inner = boxed[(k + j as usize) & 0xFF] as usize;
    boxed[(k + inner) & 0xFF]
}

fn build_keystream_ext(boxed: &[u8; 256]) -> [u8; 320] {
    let mut ext = [0u8; 320];
    // Byte n of the payload consumes step n+1, so the table is shifted by one:
    // ext[n] is the keystream byte for payload offset n within the period.
    for (i, slot) in ext.iter_mut().enumerate().take(256) {
        *slot = stream_byte(boxed, i.wrapping_add(1) as u8);
    }
    let head: [u8; 64] = ext[..64].try_into().unwrap();
    ext[256..].copy_from_slice(&head);
    ext
}

/// XORs `src` into `dst` starting at phase zero of the keystream period.
pub fn xor_payload(dst: &mut [u8], src: &[u8], ks: &[u8; 320]) {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        xor_fast(dst, src, ks);
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        xor_scalar(dst, src, ks, 0);
    }
}

fn xor_scalar(dst: &mut [u8], src: &[u8], ks: &[u8; 320], phase: usize) {
    for (i, (d, s)) in dst.iter_mut().zip(src).enumerate() {
        *d = *s ^ ks[(phase + i) & 0xFF];
    }
}

#[cfg(target_arch = "aarch64")]
fn xor_fast(dst: &mut [u8], src: &[u8], ks: &[u8; 320]) {
    use std::arch::aarch64::*;
    let len = src.len().min(dst.len());
    let mut i = 0;
    // Safety: every load reads 16 bytes at an offset that is a multiple of 64
    // within the period, and the extended table has 64 bytes of slack.
    unsafe {
        while i + 64 <= len {
            let k = ks.as_ptr().add(i & 0xFF);
            let s0 = vld1q_u8(src.as_ptr().add(i));
            let s1 = vld1q_u8(src.as_ptr().add(i + 16));
            let s2 = vld1q_u8(src.as_ptr().add(i + 32));
            let s3 = vld1q_u8(src.as_ptr().add(i + 48));
            vst1q_u8(dst.as_mut_ptr().add(i), veorq_u8(s0, vld1q_u8(k)));
            vst1q_u8(
                dst.as_mut_ptr().add(i + 16),
                veorq_u8(s1, vld1q_u8(k.add(16))),
            );
            vst1q_u8(
                dst.as_mut_ptr().add(i + 32),
                veorq_u8(s2, vld1q_u8(k.add(32))),
            );
            vst1q_u8(
                dst.as_mut_ptr().add(i + 48),
                veorq_u8(s3, vld1q_u8(k.add(48))),
            );
            i += 64;
        }
    }
    xor_scalar(&mut dst[i..len], &src[i..len], ks, i & 0xFF);
}

#[cfg(target_arch = "x86_64")]
fn xor_fast(dst: &mut [u8], src: &[u8], ks: &[u8; 320]) {
    if std::is_x86_feature_detected!("avx2") {
        // Safety: guarded by the runtime feature check above.
        unsafe { xor_avx2(dst, src, ks) }
    } else {
        xor_scalar(dst, src, ks, 0);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn xor_avx2(dst: &mut [u8], src: &[u8], ks: &[u8; 320]) {
    use std::arch::x86_64::*;
    let len = src.len().min(dst.len());
    let mut i = 0;
    while i + 32 <= len {
        let k = ks.as_ptr().add(i & 0xFF);
        let s = _mm256_loadu_si256(src.as_ptr().add(i) as *const __m256i);
        let key = _mm256_loadu_si256(k as *const __m256i);
        _mm256_storeu_si256(
            dst.as_mut_ptr().add(i) as *mut __m256i,
            _mm256_xor_si256(s, key),
        );
        i += 32;
    }
    xor_scalar(&mut dst[i..len], &src[i..len], ks, i & 0xFF);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_bytes() -> Vec<u8> {
        (0..143).map(|i| (i * 7) as u8).collect()
    }

    #[test]
    fn keystream_is_pinned() {
        let ext = build_keystream_ext(&build_keybox(&key_bytes()));
        let hex: String = ext[..16].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "7c8eadace4b9894bb3b10552f67bbc60");
    }

    #[test]
    fn period_repeats() {
        let ext = build_keystream_ext(&build_keybox(&key_bytes()));
        assert_eq!(&ext[256..320], &ext[..64]);
    }

    #[test]
    fn vectorised_path_matches_reference_for_every_length() {
        let ext = build_keystream_ext(&build_keybox(&key_bytes()));
        let src: Vec<u8> = (0..600).map(|i| (i * 31) as u8).collect();
        for len in 0..=src.len() {
            let mut fast = vec![0u8; len];
            let mut slow = vec![0u8; len];
            xor_payload(&mut fast, &src[..len], &ext);
            xor_scalar(&mut slow, &src[..len], &ext, 0);
            assert_eq!(fast, slow, "length {len}");
        }
    }

    /// Timing harness. Run with:
    /// `cargo test --release bench_decrypt -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_decrypt() {
        let ext = build_keystream_ext(&build_keybox(&key_bytes()));
        let size = 150 << 20;
        let src = std::hint::black_box(vec![0x5au8; size]);
        let mut dst = vec![0u8; size];
        for _ in 0..3 {
            xor_payload(&mut dst, &src, &ext);
        }
        let start = std::time::Instant::now();
        let rounds = 10;
        for _ in 0..rounds {
            xor_payload(std::hint::black_box(&mut dst), &src, &ext);
        }
        std::hint::black_box(&dst);
        let elapsed = start.elapsed().as_secs_f64() / rounds as f64;
        let mb = size as f64 / (1024.0 * 1024.0);
        println!(
            "{:.1} ms per 150 MiB chunk -> {:.0} MB/s",
            elapsed * 1000.0,
            mb / elapsed
        );
    }

    #[test]
    fn full_chunk_matches_reference() {
        let ext = build_keystream_ext(&build_keybox(&key_bytes()));
        let src: Vec<u8> = (0..CHUNK).map(|i| (i % 251) as u8).collect();
        let mut fast = vec![0u8; CHUNK];
        let mut slow = vec![0u8; CHUNK];
        xor_payload(&mut fast, &src, &ext);
        xor_scalar(&mut slow, &src, &ext, 0);
        assert_eq!(fast, slow);
    }
}

/// Builds containers that match the real layout, for tests only. It reuses the
/// same constants and key schedule as the parser so a fixture cannot drift from
/// the format the parser expects.
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;
    use aes::cipher::BlockEncrypt;

    fn aes_ecb_encrypt(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new(GenericArray::from_slice(key));
        let mut pad = 16 - data.len() % 16;
        if pad == 0 {
            pad = 16;
        }
        let mut plain = data.to_vec();
        plain.extend(std::iter::repeat(pad as u8).take(pad));
        for block in plain.chunks_exact_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        plain
    }

    pub struct Meta0<'a> {
        pub name: &'a str,
        pub album: &'a str,
        pub artist: &'a str,
        pub duration_ms: u64,
    }

    pub fn build(payload: &[u8], meta: Option<Meta0>, cover: &[u8], cover_pad: usize) -> Vec<u8> {
        let stream_key: Vec<u8> = (0..160).map(|i| ((i * 13 + 7) % 251) as u8).collect();
        let mut key_cipher = aes_ecb_encrypt(&CORE_KEY, &stream_key);
        for byte in key_cipher.iter_mut() {
            *byte ^= 0x64;
        }

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&[0x01, 0x6d]);
        out.extend_from_slice(&(key_cipher.len() as u32).to_le_bytes());
        out.extend_from_slice(&key_cipher);

        match meta {
            Some(meta) => {
                let json = format!(
                    r#"{{"musicName":"{}","album":"{}","artist":[["{}","1"]],"duration":{}}}"#,
                    meta.name, meta.album, meta.artist, meta.duration_ms
                );
                let mut plain = META_JSON_TAG.to_vec();
                plain.extend_from_slice(json.as_bytes());
                let mut blob = aes_ecb_encrypt(&META_KEY, &plain);
                let encoded = base64::engine::general_purpose::STANDARD.encode(&blob);
                let mut body = META_PLAIN_TAG.to_vec();
                body.extend_from_slice(encoded.as_bytes());
                for byte in body.iter_mut() {
                    *byte ^= 0x63;
                }
                blob = body;
                out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
                out.extend_from_slice(&blob);
            }
            None => out.extend_from_slice(&0u32.to_le_bytes()),
        }

        out.extend_from_slice(&[0u8; 4]); // crc32
        out.push(1); // cover frame version
        out.extend_from_slice(&((cover.len() + cover_pad) as u32).to_le_bytes());
        out.extend_from_slice(&(cover.len() as u32).to_le_bytes());
        out.extend_from_slice(cover);
        out.extend(std::iter::repeat(0u8).take(cover_pad));

        let ks = build_keystream_ext(&build_keybox(&stream_key[KEYBOX_PREFIX..]));
        let mut encrypted = Vec::with_capacity(payload.len());
        encrypted.extend(payload.iter().enumerate().map(|(i, b)| b ^ ks[i & 0xFF]));
        out.extend_from_slice(&encrypted);
        out
    }
}
