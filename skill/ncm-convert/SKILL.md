---
name: ncm-convert
description: Use when the user wants NetEase Cloud Music .ncm files decrypted or converted to mp3, flac, wav, m4a or mp4, including requests like "把这些 ncm 转了", "转成 mp3", "批量转一下这个文件夹", or preparing local music for upload to an LLM. Drives the local `ncmconv` CLI and never implements decryption itself.
---

# ncmconv

## Preconditions

Check before doing anything:

```sh
command -v ncmconv     # missing -> see Install
command -v ffmpeg      # only needed when -format is not original
```

Never reimplement decryption, the key schedule, or AES blocks. The CLI already
holds a verified implementation; a second one diverges silently and cannot be
checked against real files.

## Commands

```sh
# Look before converting: payload format, duration, tags, cover
ncmconv -info <files-or-dirs>

# Archive: bit-exact export, no encoder involved
ncmconv -o <dir> <files-or-dirs>

# Archive with usable tags: audio copied, only the container rewritten
ncmconv -tag -o <dir> <files-or-dirs>

# Upload to an LLM or hand to someone: 320 kbps mp3
ncmconv -format mp3 -b 320k -o <dir> <files-or-dirs>

# Recursive batch that leaves a machine-readable record
ncmconv -r -format mp3 -manifest <dir>/manifest.json -o <dir> <music-dir>
```

`-tag` matters for a music library: without it the export is the raw payload,
which often carries no title, artist or cover at all. It remuxes with stream
copy, so the audio is still untouched — offer it whenever the result is going
into a player rather than into an analyser.

Run `-info` first when the user is unsure what they have: it reports the payload
container, duration, tags and whether a cover exists, and writes nothing.

Choose the format from the intent, not from convenience:

| Intent | Flag |
|---|---|
| keep the best copy, feed an editor | `-format original` (`wav` only if a tool demands PCM) |
| upload to a chat LLM, send to a person | `-format mp3 -b 320k` |
| smallest file for speech or analysis | `-format m4a -b 128k` |

`mp4` is the same AAC bitstream as `m4a`; pick it only when the destination
insists on a `.mp4` extension. Never convert to `wav` "for quality": it is
larger with zero additional information, and only justified for PCM-only tools.

## Rules

- **Never delete or move `.ncm` sources.** The CLI never does, and neither should you.
- **Re-running is safe.** Existing outputs are skipped; pass `-overwrite` only when the user asks for a redo.
- **Read the exit code.** `1` means some files failed; name them in the report.
- **Do not dump the manifest.** Report the converted count and the output
  directory, and quote at most a couple of rows.
- **Flag pointless work instead of silently performing it.** If the source is
  already lossy and the user asks for the same lossy family, say that
  re-encoding cannot add detail and offer the bit-exact copy. `-force` is the
  escape hatch when they genuinely want a re-encode anyway (one bitrate for a
  whole library, for example).

## Install

If `command -v ncmconv` fails and the checkout exists at
`~/Documents/Project/ncmconv`:

```sh
cd ~/Documents/Project/ncmconv && cargo build --release && cp target/release/ncmconv ~/.local/bin/
```

Otherwise `cargo install --git https://github.com/ByteFluffy/ncmconv`.

## Legal

Personal use on files the user already has. Do not help redistribute decrypted
audio, and do not embed this in a product without raising the DRM question.
