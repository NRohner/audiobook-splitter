# Album Artwork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every file a split writes carries album artwork: the cover embedded in the source (default), an image the user attaches, or none.

**Architecture:** `Metadata` gets a `cover: Cover` choice. A new `cover` module turns the choice into one image file (extracting the embedded cover once if needed). `write_segment` adds that image as an extra FFmpeg input on every cut and join with `attached_pic`. The CLI flag, interactive editor and GUI all set the same field.

**Tech Stack:** Rust 2024, FFmpeg/FFprobe (already required), clap, dialoguer, tiny_http GUI with `src/ui.html`. No new dependencies.

Spec: `docs/superpowers/specs/2026-11-16-album-artwork-design.md`

## Global Constraints

- No new Cargo dependencies.
- Images are JPEG or PNG only, detected by magic bytes (never by file name).
- Default choice is `Keep` (reuse the source's embedded cover); an explicit image or `none` overrides it.
- Only MP3 and M4A/M4B outputs get artwork; other containers skip it with one log line and still split.
- A bad cover file fails before any output file is written (`prepare_split`).
- A cover that cannot be extracted under `Keep` is a log line, not an error.
- When a cover is attached, `-ss`/`-t` are input options on the audio `-i` only (an output-side `-ss` drops the image's single frame, so the output silently has no cover).
- `split_file` (older start/end splitter) keeps its current behavior.
- No emojis in any UI (SVG icons or text only).
- Match surrounding style: `//` comments above items, compact one-line fns where the file already does that. Do not run `cargo fmt` over existing files.
- Commit after every task on branch `feat/cli-updates`. End commit messages with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `src/meta.rs` | modify | `Cover` enum and `Metadata.cover` |
| `src/cover.rs` | create | sniff/validate images, find and extract embedded cover, `resolve` a choice to a path |
| `src/testutil.rs` | modify | test helpers: make images, covered audio, read a cover's colour |
| `src/lib.rs` | modify | `Art` enum, `cut`/`concat`/`join`/`write_segment` take a cover; `pub mod cover` |
| `src/pipeline.rs` | modify | `prepare_split` validates; `write_tracks` resolves and passes the cover |
| `src/cli/split.rs` | modify | pass files and cover to `write_tracks` |
| `src/cli/mod.rs` | modify | `--cover` flag on `Tags` |
| `src/cli/edit.rs` | modify | `set_cover` edit and the "Cover art" prompt |
| `src/cli/analyze.rs` | modify | "Cover:" line in the summary |
| `src/gui.rs` | modify | image picker kind, thumbnail, `cover_source`, pass cover to `write_tracks` |
| `src/ui.html` | modify | "Cover art" controls |
| `tests/common/mod.rs`, `tests/cli.rs` | modify | end-to-end CLI tests |
| `README.md` | modify | document the feature |

---

### Task 1: The `Cover` choice on `Metadata`

**Files:**
- Modify: `src/meta.rs`

**Interfaces:**
- Produces: `meta::Cover { Keep (default), None, File(PathBuf) }` (derives `Debug, Clone, Default, PartialEq, Serialize, Deserialize`; serializes as `"keep"`, `"none"`, `{"file": "<path>"}`) and `Metadata.cover: Cover`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/meta.rs`:

```rust
    #[test]
    fn cover_serializes_as_keep_none_or_file() {
        assert_eq!(serde_json::to_string(&Cover::Keep).unwrap(), r#""keep""#);
        assert_eq!(serde_json::to_string(&Cover::None).unwrap(), r#""none""#);
        assert_eq!(serde_json::to_string(&Cover::File("a.png".into())).unwrap(), r#"{"file":"a.png"}"#);
        let back: Cover = serde_json::from_str(r#"{"file":"a.png"}"#).unwrap();
        assert_eq!(back, Cover::File("a.png".into()));
    }

    #[test]
    fn metadata_without_a_cover_field_keeps_the_embedded_cover() {
        let m: Metadata = serde_json::from_str(r#"{"album":"A"}"#).unwrap();
        assert_eq!(m.cover, Cover::Keep);
        assert_eq!(Metadata::default().cover, Cover::Keep);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib meta::`
Expected: compile error, `cannot find type 'Cover' in this scope`.

- [ ] **Step 3: Implement**

In `src/meta.rs`, add `use std::path::PathBuf;` below the existing `use serde...` line, add the enum above `Metadata`, and the field:

```rust
// Where every output file's album artwork comes from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cover {
    #[default]
    Keep, // The cover embedded in the source file(s), if there is one.
    None,
    File(PathBuf), // A JPEG or PNG the user chose.
}
```

Add to `struct Metadata`, after `template`:

```rust
    pub cover: Cover,
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib meta::`
Expected: all `meta::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add src/meta.rs docs/superpowers/specs/2026-11-16-album-artwork-design.md docs/superpowers/plans/2026-11-16-album-artwork.md
git commit -m "Add the Cover choice to Metadata, with the artwork spec and plan"
```

---

### Task 2: The `cover` module and test helpers

**Files:**
- Create: `src/cover.rs`
- Modify: `src/lib.rs` (add `pub mod cover;` after `pub mod chapters;`)
- Modify: `src/testutil.rs`

**Interfaces:**
- Consumes: `meta::Cover`, `chapters::AudioFile`, `pipeline::file_name(&Path) -> String`.
- Produces (all in `crate::cover`):
  - `supported(ext: &str) -> bool` (mp3, m4a, m4b, case-insensitive)
  - `sniff(path: &Path) -> Result<&'static str, String>` ("jpg" or "png")
  - `validate(cover: &Cover) -> Result<(), String>`
  - `find_embedded(files: &[AudioFile]) -> Option<(usize, String)>` (file index, codec name)
  - `resolve(cover: &Cover, files: &[AudioFile]) -> Result<Resolved, String>`
  - `Resolved { pub note: Option<String> }` with `path(&self) -> Option<&Path>`; deletes its temp file on drop.
- Produces (in `crate::testutil`, test builds only): `image(path, color)`, `audio_with_cover(path, seconds, cover)`, `cover_color(path) -> Option<&'static str>`, `tag(path, key) -> String`.

- [ ] **Step 1: Add the test helpers**

Append to `src/testutil.rs`:

```rust

// A 64x64 picture of one colour ("red", "green", "blue"); the extension picks JPEG or PNG.
pub fn image(path: &Path, color: &str) {
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c={}:s=64x64", color), "-frames:v", "1", "-y"])
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

// A sine-tone MP3 or M4B (by extension), with `cover` embedded if given.
pub fn audio_with_cover(path: &Path, seconds: u32, cover: Option<&Path>) {
    let codec = if path.extension().is_some_and(|e| e == "mp3") { "libmp3lame" } else { "aac" };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("sine=d={}", seconds)]);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover).args(["-map", "0:a", "-map", "1:v", "-c:v", "copy", "-disposition:v:0", "attached_pic"]);
    }
    let made = cmd.args(["-c:a", codec, "-y"]).arg(path).status().unwrap();
    assert!(made.success());
}

// The dominant colour of the first picture in `path`, or None if it has no picture.
pub fn cover_color(path: &Path) -> Option<&'static str> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf", "scale=1:1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    let pixel = out.stdout.get(..3)?;
    let strongest = pixel.iter().enumerate().max_by_key(|(_, v)| **v)?.0;
    Some(["red", "green", "blue"][strongest])
}

// A format tag such as "title", or "" if there is none.
pub fn tag(path: &Path, key: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", &format!("format_tags={}", key), "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
```

- [ ] **Step 2: Write `src/cover.rs` with failing tests only**

Create `src/cover.rs` containing just the test module below (implementation comes in Step 4), and add `pub mod cover;` to `src/lib.rs` after `pub mod chapters;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{audio_with_cover, cover_color, image, temp_dir};

    fn audio(path: std::path::PathBuf) -> AudioFile {
        AudioFile { path, silences: vec![], duration: 6.0 }
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn sniff_reads_the_bytes_not_the_name() {
        let dir = temp_dir("cover-sniff");
        write(&dir.join("a.png"), &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]); // JPEG bytes named .png
        write(&dir.join("b.jpg"), &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]);
        write(&dir.join("c.png"), b"GIF89a....");
        write(&dir.join("d.png"), b"");
        assert_eq!(sniff(&dir.join("a.png")), Ok("jpg"));
        assert_eq!(sniff(&dir.join("b.jpg")), Ok("png"));
        assert!(sniff(&dir.join("c.png")).unwrap_err().contains("not a JPEG or PNG"));
        assert!(sniff(&dir.join("d.png")).is_err());
        assert!(sniff(&dir.join("missing.png")).unwrap_err().contains("missing.png"));
    }

    #[test]
    fn validate_only_checks_a_chosen_file() {
        assert!(validate(&Cover::Keep).is_ok());
        assert!(validate(&Cover::None).is_ok());
        assert!(validate(&Cover::File("nope.png".into())).is_err());
    }

    #[test]
    fn only_mp3_and_m4_containers_take_artwork() {
        assert!(supported("mp3") && supported("M4B") && supported("m4a"));
        assert!(!supported("wav") && !supported("flac") && !supported("ogg"));
    }

    #[test]
    fn jpeg_and_png_codecs_map_to_extensions() {
        assert_eq!(ext_for("mjpeg"), Some("jpg"));
        assert_eq!(ext_for("png"), Some("png"));
        assert_eq!(ext_for("bmp"), None);
    }

    #[test]
    fn finds_the_first_file_with_a_cover() {
        let dir = temp_dir("cover-find");
        let red = dir.join("red.jpg");
        image(&red, "red");
        let (plain, covered) = (dir.join("a.mp3"), dir.join("b.m4b"));
        audio_with_cover(&plain, 6, None);
        audio_with_cover(&covered, 6, Some(&red));
        assert_eq!(find_embedded(&[audio(plain.clone())]), None);
        assert_eq!(find_embedded(&[audio(plain), audio(covered)]), Some((1, "mjpeg".to_string())));
    }

    #[test]
    fn keep_extracts_the_cover_and_cleans_up() {
        let dir = temp_dir("cover-keep");
        let red = dir.join("red.jpg");
        image(&red, "red");
        let book = dir.join("book.m4b");
        audio_with_cover(&book, 6, Some(&red));
        let resolved = resolve(&Cover::Keep, &[audio(book)]).unwrap();
        let path = resolved.path().unwrap().to_path_buf();
        assert_eq!(cover_color(&path), Some("red"));
        assert!(resolved.note.is_none());
        drop(resolved);
        assert!(!path.exists(), "the extracted cover is removed on drop");
    }

    #[test]
    fn keep_with_no_embedded_cover_is_quietly_nothing() {
        let dir = temp_dir("cover-keep-none");
        let book = dir.join("book.mp3");
        audio_with_cover(&book, 6, None);
        let resolved = resolve(&Cover::Keep, &[audio(book)]).unwrap();
        assert!(resolved.path().is_none() && resolved.note.is_none());
    }

    #[test]
    fn none_and_file_resolve_as_chosen() {
        let dir = temp_dir("cover-choose");
        let blue = dir.join("blue.png");
        image(&blue, "blue");
        assert!(resolve(&Cover::None, &[]).unwrap().path().is_none());
        let resolved = resolve(&Cover::File(blue.clone()), &[]).unwrap();
        assert_eq!(resolved.path(), Some(blue.as_path()));
        drop(resolved);
        assert!(blue.exists(), "a chosen image is never deleted");
        assert!(resolve(&Cover::File(dir.join("nope.png")), &[]).is_err());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib cover::`
Expected: compile errors, `cannot find function 'sniff'` and similar.

- [ ] **Step 4: Implement (put above the test module in `src/cover.rs`)**

```rust
// Turns the user's cover choice into one image file every output can attach.
use crate::chapters::AudioFile;
use crate::meta::Cover;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

// Containers FFmpeg can attach a picture to with stream copy.
pub fn supported(ext: &str) -> bool {
    ["mp3", "m4a", "m4b"].contains(&ext.to_lowercase().as_str())
}

// "jpg" or "png", from the file's first bytes (its name may lie).
pub fn sniff(path: &Path) -> Result<&'static str, String> {
    let mut head = [0u8; 8];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("Can't read the cover image '{}': {}", path.display(), e))?;
    match &head[..n] {
        [0xFF, 0xD8, 0xFF, ..] => Ok("jpg"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] => Ok("png"),
        _ => Err(format!("'{}' is not a JPEG or PNG image.", path.display())),
    }
}

// Fails on a chosen image that is missing or not a JPEG/PNG, before anything is written.
pub fn validate(cover: &Cover) -> Result<(), String> {
    match cover {
        Cover::File(path) => sniff(path).map(|_| ()),
        _ => Ok(()),
    }
}

fn ext_for(codec: &str) -> Option<&'static str> {
    match codec {
        "mjpeg" => Some("jpg"),
        "png" => Some("png"),
        _ => None,
    }
}

// The first file with a picture stream, and that stream's codec. Any picture stream counts,
// not only `attached_pic`: Audible-style M4Bs keep the cover as a plain MJPEG track.
pub fn find_embedded(files: &[AudioFile]) -> Option<(usize, String)> {
    files.iter().enumerate().find_map(|(i, f)| Some((i, picture_codec(&f.path)?)))
}

fn picture_codec(path: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|codec| !codec.is_empty())
}

// Copies the first picture stream of `source` to `dest` without re-encoding.
fn extract(source: &Path, dest: &Path) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-map", "0:v:0", "-frames:v", "1", "-c", "copy", "-y"])
        .arg(dest)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;
    if out.status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_file(dest);
    Err(String::from_utf8_lossy(&out.stderr).into_owned())
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch_path(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("abs-cover-{}-{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), ext))
}

// The image to attach to every output (if any). Dropping it deletes an extracted copy.
pub struct Resolved {
    path: Option<PathBuf>,
    temp: bool,               // The file is ours to delete.
    pub note: Option<String>, // Why `Keep` ended up with no artwork, when the user should hear it.
}

impl Resolved {
    fn none(note: Option<String>) -> Resolved {
        Resolved { path: None, temp: false, note }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Drop for Resolved {
    fn drop(&mut self) {
        if let Some(path) = self.path.as_ref().filter(|_| self.temp) {
            let _ = std::fs::remove_file(path);
        }
    }
}

// Errors only for a chosen image that is unusable; `Keep` problems come back as a note.
pub fn resolve(cover: &Cover, files: &[AudioFile]) -> Result<Resolved, String> {
    match cover {
        Cover::None => Ok(Resolved::none(None)),
        Cover::File(path) => {
            sniff(path)?;
            Ok(Resolved { path: Some(path.clone()), temp: false, note: None })
        }
        Cover::Keep => {
            let Some((i, codec)) = find_embedded(files) else { return Ok(Resolved::none(None)) };
            let name = crate::pipeline::file_name(&files[i].path);
            let Some(ext) = ext_for(&codec) else {
                return Ok(Resolved::none(Some(format!("The cover in {} is {}, not JPEG or PNG; writing no artwork.", name, codec))));
            };
            let dest = scratch_path(ext);
            match extract(&files[i].path, &dest) {
                Ok(()) => Ok(Resolved { path: Some(dest), temp: true, note: None }),
                Err(e) => Ok(Resolved::none(Some(format!("Couldn't read the cover from {}; writing no artwork.\n{}", name, e)))),
            }
        }
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib cover::`
Expected: 8 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/cover.rs src/lib.rs src/testutil.rs
git commit -m "Add the cover module: validate images, find, extract and resolve covers"
```

---

### Task 3: Attach the cover when writing files

**Files:**
- Modify: `src/lib.rs` (`cut`, `concat`, `join`, `write_segment`, `split_file`, tests)
- Modify: `src/pipeline.rs` (`prepare_split`, `write_tracks`, tests)
- Modify: `src/cli/split.rs`, `src/gui.rs` (call sites)

**Interfaces:**
- Consumes: `cover::{supported, validate, resolve, Resolved}`, `meta::Cover`.
- Produces:
  - `lib::write_segment(pieces: &[chapters::Piece], out_dir: &Path, track: &meta::Track, cover: Option<&Path>) -> Result<PathBuf, String>`
  - `pipeline::write_tracks(segments: &[Segment], names: &[Track], out_dir: &Path, files: &[AudioFile], cover: &Cover, emit: &mut dyn FnMut(Event)) -> SplitReport`
  - `pipeline::prepare_split` now also returns `Err` for an unusable `Cover::File`.

- [ ] **Step 1: Write the failing `write_segment` tests**

Add to `mod tests` in `src/lib.rs` (after the existing tests; `use crate::testutil;` goes at the top of that module next to `use std::fs;`):

```rust
    fn piece(path: &Path, start: f64, end: f64) -> chapters::Piece {
        chapters::Piece { path: path.to_path_buf(), start, end }
    }

    fn track(name: &str) -> meta::Track {
        meta::Track { name: name.into(), tags: vec![("title".into(), "Chapter".into())] }
    }

    // A 6 s book of each type with a red cover, plus a blue PNG to swap in.
    fn covered_books(name: &str) -> (PathBuf, Vec<PathBuf>, PathBuf) {
        let dir = temp_dir(name);
        let (red, blue) = (dir.join("red.jpg"), dir.join("blue.png"));
        testutil::image(&red, "red");
        testutil::image(&blue, "blue");
        let books: Vec<PathBuf> = ["mp3", "m4b"].iter().map(|ext| dir.join(format!("book.{}", ext))).collect();
        for book in &books {
            testutil::audio_with_cover(book, 6, Some(&red));
        }
        (dir, books, blue)
    }

    #[test]
    fn write_segment_attaches_the_chosen_cover() {
        let (dir, books, blue) = covered_books("seg-attach");
        for book in &books {
            let out = write_segment(&[piece(book, 1.0, 4.0)], &dir, &track("t"), Some(&blue)).unwrap();
            assert_eq!(testutil::cover_color(&out), Some("blue"), "{}", out.display());
            assert!((duration(&out) - 3.0).abs() < 0.15, "{} is {}s", out.display(), duration(&out));
            assert_eq!(testutil::tag(&out, "title"), "Chapter");
        }
    }

    #[test]
    fn write_segment_without_a_cover_strips_the_source_one() {
        let (dir, books, _) = covered_books("seg-strip");
        for book in &books {
            let out = write_segment(&[piece(book, 1.0, 4.0)], &dir, &track("t"), None).unwrap();
            assert_eq!(testutil::cover_color(&out), None, "{}", out.display());
            assert!((duration(&out) - 3.0).abs() < 0.15);
        }
    }

    #[test]
    fn a_chapter_joined_from_two_files_carries_the_cover() {
        let (dir, books, blue) = covered_books("seg-join");
        for book in &books {
            let ext = book.extension().unwrap().to_string_lossy().to_string();
            let second = dir.join(format!("second.{}", ext));
            fs::copy(book, &second).unwrap();
            let pieces = [piece(book, 1.0, 4.0), piece(&second, 0.0, 2.0)];
            let with = write_segment(&pieces, &dir, &track("with"), Some(&blue)).unwrap();
            assert_eq!(testutil::cover_color(&with), Some("blue"), "{}", with.display());
            assert!((duration(&with) - 5.0).abs() < 0.2, "{}s", duration(&with));
            let without = write_segment(&pieces, &dir, &track("without"), None).unwrap();
            assert_eq!(testutil::cover_color(&without), None, "{}", without.display());
        }
    }

    #[test]
    fn write_segment_leaves_other_containers_alone() {
        let dir = temp_dir("seg-wav");
        let (wav, blue) = (dir.join("book.wav"), dir.join("blue.png"));
        testutil::tone_with_gaps(&wav, 6, &[]);
        testutil::image(&blue, "blue");
        let out = write_segment(&[piece(&wav, 1.0, 4.0)], &dir, &track("t"), Some(&blue)).unwrap();
        assert_eq!(testutil::cover_color(&out), None);
        assert!((duration(&out) - 3.0).abs() < 0.15);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib write_segment`
Expected: compile error, `this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Implement the FFmpeg changes in `src/lib.rs`**

Replace `split_file`'s `cut(...)` call and the functions `write_segment`, `join`, `cut`, `concat` with the versions below. In `split_file` change the call to `cut(input, start, duration, &[], Art::Untouched, &out_path)`.

```rust
// Writes one chapter, joined from `pieces` with stream copy, to `<out_dir>/<track.name>.<ext>`
// (never overwriting an existing file) and tags it. The pieces must share a codec.
// `cover` is attached to MP3 and M4A/M4B outputs; with None those outputs carry no artwork.
pub fn write_segment(pieces: &[chapters::Piece], out_dir: &Path, track: &meta::Track, cover: Option<&Path>) -> Result<PathBuf, String> {
    let first = pieces.first().ok_or("A chapter has no audio.")?;
    let ext = first.path.extension().unwrap_or_default().to_string_lossy().to_string();
    let out_path = unique_path(out_dir, &track.name, &ext);
    let result = if !cover::supported(&ext) {
        match pieces {
            [p] => cut(&p.path, p.start, p.end - p.start, &track.tags, Art::Untouched, &out_path),
            _ => join(pieces, &track.tags, None, &out_path, &ext),
        }
    } else {
        match (pieces, cover) {
            ([p], Some(c)) => cut(&p.path, p.start, p.end - p.start, &track.tags, Art::Attach(c), &out_path),
            ([p], None) => cut(&p.path, p.start, p.end - p.start, &track.tags, Art::Strip, &out_path),
            _ => join(pieces, &track.tags, cover, &out_path, &ext),
        }
    };
    result.map(|_| out_path).map_err(|e| format!("FFmpeg failed on '{}':\n{}", track.name, e))
}

// Cuts each piece to a temporary file beside `out`, then joins them in order. (The concat
// demuxer's own `inpoint` seeks on an M4B's cover track, so it would start from 0.)
fn join(pieces: &[chapters::Piece], tags: &[(String, String)], cover: Option<&Path>, out: &Path, ext: &str) -> Result<(), String> {
    let parts: Vec<PathBuf> = (0..pieces.len()).map(|i| out.with_extension(format!("part{}.{}", i, ext))).collect();
    let result = pieces
        .iter()
        .zip(&parts)
        .try_for_each(|(p, part)| cut(&p.path, p.start, p.end - p.start, &[], Art::Strip, part))
        .and_then(|_| concat(&parts, &pieces[0].path, tags, cover, out));
    for part in &parts {
        let _ = std::fs::remove_file(part);
    }
    result
}

// What a cut does with cover art.
enum Art<'a> {
    Untouched,        // FFmpeg's defaults (the older `split_file` path, and containers without artwork).
    Strip,            // Audio only.
    Attach(&'a Path), // Audio plus this image as the cover.
}

// Cuts `duration` seconds of `input` from `start` into `out` with stream copy and tags it.
// Returns FFmpeg's error output on failure.
fn cut(input: &Path, start: f64, duration: f64, tags: &[(String, String)], art: Art, out: &Path) -> Result<(), String> {
    let (start, duration) = (start.to_string(), duration.to_string());
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    match art {
        Art::Attach(cover) => {
            // The seek goes on the audio input: an output-side -ss would also skip the
            // image's single frame, leaving a file with no cover and no error.
            cmd.args(["-ss", &start, "-t", &duration, "-i"])
                .arg(input)
                .arg("-i")
                .arg(cover)
                .args(["-map", "0:a", "-map", "1:v", "-c", "copy", "-disposition:v:0", "attached_pic"]);
        }
        _ => {
            cmd.arg("-i").arg(input).args(["-ss", &start, "-t", &duration, "-c", "copy"]);
            if matches!(art, Art::Strip) {
                cmd.args(["-map", "0:a"]);
            } else {
                // ponytail: drops cover art from M4B/M4A parts; the ipod muxer rejects cover
                // tracks stored as MJPEG video. `write_segment` attaches covers instead.
                let ext = input.extension().unwrap_or_default().to_string_lossy().to_lowercase();
                if ["m4b", "m4a"].contains(&ext.as_str()) {
                    cmd.arg("-vn");
                }
            }
        }
    }
    for (key, value) in tags {
        cmd.arg("-metadata").arg(format!("{}={}", key, value));
    }
    finish(cmd.arg("-y").arg(out).stdin(Stdio::null()).output(), out)
}

// Joins `parts` into `out` with stream copy, keeping `source`'s tags and adding `tags`
// and, if given, `cover` as the artwork.
fn concat(parts: &[PathBuf], source: &Path, tags: &[(String, String)], cover: Option<&Path>, out: &Path) -> Result<(), String> {
    // A concat demuxer list on stdin, with `file:` paths so they don't resolve against `pipe:`;
    // inside quotes only ' needs escaping, as '\''.
    let list: String = parts
        .iter()
        .map(|p| {
            let path = std::path::absolute(p).unwrap_or_else(|_| p.clone());
            format!("file 'file:{}'\n", path.to_string_lossy().replace('\'', r"'\''"))
        })
        .collect();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-protocol_whitelist", "file,pipe", "-i", "pipe:0", "-i"])
        .arg(source);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover).args(["-map", "0:a", "-map", "2:v"]);
    } else {
        cmd.args(["-map", "0:a"]);
    }
    cmd.args(["-map_metadata", "1", "-c", "copy"]);
    if cover.is_some() {
        cmd.args(["-disposition:v:0", "attached_pic"]);
    }
    for (key, value) in tags {
        cmd.arg("-metadata").arg(format!("{}={}", key, value));
    }
    let mut child = cmd
        .arg("-y")
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;
    let _ = child.stdin.take().unwrap().write_all(list.as_bytes()); // Dropping it ends the list.
    finish(child.wait_with_output(), out)
}
```

- [ ] **Step 4: Fix the compile errors this causes elsewhere, then write the pipeline tests**

In `src/pipeline.rs`:

1. Change the imports: `use crate::meta::{tracks, Cover, Metadata, Track, DEFAULT_TEMPLATE};` and `use crate::{cover, detect_silences, detect_silences_between, split_points, write_segment};`.
2. Replace `prepare_split`:

```rust
pub fn prepare_split(files: &[AudioFile], breaks: &[Break], meta: &Metadata) -> Result<(Vec<Segment>, Vec<Track>), String> {
    cover::validate(&meta.cover)?;
    let segments = plan_from_breaks(files, breaks)?;
    let names = tracks(&segments, meta)?;
    Ok((segments, names))
}
```

3. Replace `write_tracks`:

```rust
// Writes one tagged file per chapter into `out_dir` (which must exist), each carrying the
// artwork `cover` picks (the embedded cover is read from `files`).
pub fn write_tracks(
    segments: &[Segment],
    names: &[Track],
    out_dir: &Path,
    files: &[AudioFile],
    cover: &Cover,
    emit: &mut dyn FnMut(Event),
) -> SplitReport {
    let mut report = SplitReport { written: Vec::new(), failed: 0 };
    let resolved = match cover::resolve(cover, files) {
        Ok(resolved) => resolved,
        Err(e) => {
            emit(Event::Log(e));
            report.failed = segments.len();
            return report;
        }
    };
    if let Some(note) = &resolved.note {
        emit(Event::Log(note.clone()));
    }
    let mut skipped: Vec<String> = segments
        .iter()
        .filter_map(|s| s.pieces.first()?.path.extension())
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| !cover::supported(e))
        .collect();
    skipped.sort();
    skipped.dedup();
    if resolved.path().is_some() && !skipped.is_empty() {
        emit(Event::Log(format!("Cover art is only written to MP3 and M4A/M4B files; skipped for .{}.", skipped.join(", ."))));
    }

    emit(Event::Stage { name: "Writing files".into(), total: segments.len() });
    for (i, (segment, track)) in segments.iter().zip(names).enumerate() {
        match write_segment(&segment.pieces, out_dir, track, resolved.path()) {
            Ok(path) => {
                emit(Event::Log(format!("Wrote {}", file_name(&path))));
                report.written.push(path);
            }
            Err(e) => {
                emit(Event::Log(e));
                report.failed += 1;
            }
        }
        emit(Event::Tick(i + 1));
    }
    report
}
```

4. In `src/cli/split.rs::write_output`, change the call to:

```rust
    let report = write_tracks(&segments, &names, out, &plan.files, &plan.metadata.cover, &mut |e| reporter.emit(e));
```

5. In `src/gui.rs::start_split`, make the locked block also return the files and keep the cover:

```rust
    let cover = req.meta.cover.clone();
    let (plan, names, files) = {
        let mut s = state.lock().unwrap();
        // ...existing checks unchanged...
        let (plan, names) = pipeline::prepare_split(&s.analyzed, &req.breaks, &req.meta)?;
        // ...existing create_dir_all / status updates unchanged...
        (plan, names, s.analyzed.clone())
    };
```

and the thread call becomes:

```rust
        pipeline::write_tracks(&plan, &names, &output, &files, &cover, &mut |e| apply_event(&state, e));
```

6. In the existing pipeline test `splitting_names_each_file_after_its_chapter`, change the import to `use crate::meta::{Cover, DEFAULT_TEMPLATE};` and the call to `write_tracks(&segments, &names, &out, &a.files, &Cover::default(), &mut |e| ...)`. Fix any other `write_tracks` caller the compiler reports the same way.

Add these tests to `mod tests` in `src/pipeline.rs` (add `use crate::chapters::{AudioFile, Piece, Segment}; use crate::testutil::{audio_with_cover, cover_color, image};` to its imports):

```rust
    // Two 3 s chapters of one 6 s MP3 that carries a red cover, plus a blue PNG.
    fn covered_book(name: &str) -> (std::path::PathBuf, Vec<AudioFile>, Vec<Segment>, Vec<Track>, std::path::PathBuf) {
        let dir = temp_dir(name);
        let (red, blue, book) = (dir.join("red.jpg"), dir.join("blue.png"), dir.join("book.mp3"));
        image(&red, "red");
        image(&blue, "blue");
        audio_with_cover(&book, 6, Some(&red));
        let files = vec![AudioFile { path: book.clone(), silences: vec![], duration: 6.0 }];
        let segments: Vec<Segment> = [("One", 0.0, 3.0), ("Two", 3.0, 6.0)]
            .iter()
            .map(|&(name, start, end)| Segment { name: name.into(), pieces: vec![Piece { path: book.clone(), start, end }] })
            .collect();
        let meta = Metadata { template: DEFAULT_TEMPLATE.into(), ..Default::default() };
        let names = tracks(&segments, &meta).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        (out, files, segments, names, blue)
    }

    fn colors(cover: Cover) -> (Vec<Option<&'static str>>, Vec<String>) {
        let (out, files, segments, names, _) = covered_book("pipe-colors");
        let cover = match cover {
            Cover::File(_) => Cover::File(out.parent().unwrap().join("blue.png")),
            other => other,
        };
        let mut log = Vec::new();
        let report = write_tracks(&segments, &names, &out, &files, &cover, &mut |e| {
            if let Event::Log(l) = e {
                log.push(l)
            }
        });
        assert_eq!((report.written.len(), report.failed), (2, 0), "{:?}", log);
        (report.written.iter().map(|p| cover_color(p)).collect(), log)
    }

    #[test]
    fn keep_reuses_the_source_cover_on_every_file() {
        assert_eq!(colors(Cover::Keep).0, [Some("red"), Some("red")]);
    }

    #[test]
    fn a_chosen_image_replaces_the_source_cover() {
        assert_eq!(colors(Cover::File("placeholder".into())).0, [Some("blue"), Some("blue")]);
    }

    #[test]
    fn none_writes_files_without_artwork() {
        assert_eq!(colors(Cover::None).0, [None, None]);
    }

    #[test]
    fn keep_on_a_book_without_a_cover_still_splits() {
        let dir = temp_dir("pipe-nocover");
        let book = dir.join("book.mp3");
        audio_with_cover(&book, 6, None);
        let files = vec![AudioFile { path: book.clone(), silences: vec![], duration: 6.0 }];
        let segments = vec![Segment { name: "One".into(), pieces: vec![Piece { path: book, start: 0.0, end: 3.0 }] }];
        let names = tracks(&segments, &Metadata { template: DEFAULT_TEMPLATE.into(), ..Default::default() }).unwrap();
        let report = write_tracks(&segments, &names, &dir, &files, &Cover::Keep, &mut |_| {});
        assert_eq!((report.written.len(), report.failed), (1, 0));
        assert_eq!(cover_color(&report.written[0]), None);
    }

    #[test]
    fn other_containers_get_one_log_line_about_skipped_artwork() {
        let dir = temp_dir("pipe-wav-cover");
        let (wav, blue) = (dir.join("book.wav"), dir.join("blue.png"));
        tone_with_gaps(&wav, 6, &[]);
        image(&blue, "blue");
        let files = vec![AudioFile { path: wav.clone(), silences: vec![], duration: 6.0 }];
        let segments: Vec<Segment> = [(0.0, 3.0), (3.0, 6.0)]
            .iter()
            .enumerate()
            .map(|(i, &(start, end))| Segment { name: format!("C{}", i), pieces: vec![Piece { path: wav.clone(), start, end }] })
            .collect();
        let names = tracks(&segments, &Metadata { template: DEFAULT_TEMPLATE.into(), ..Default::default() }).unwrap();
        let mut log = Vec::new();
        let report = write_tracks(&segments, &names, &dir, &files, &Cover::File(blue), &mut |e| {
            if let Event::Log(l) = e {
                log.push(l)
            }
        });
        assert_eq!(report.written.len(), 2);
        assert_eq!(log.iter().filter(|l| l.contains("only written to MP3 and M4A/M4B")).count(), 1, "{:?}", log);
    }

    #[test]
    fn a_bad_cover_fails_before_anything_is_planned() {
        let dir = temp_dir("pipe-badcover");
        let a = analyze(&[{ let w = dir.join("b.wav"); tone_with_gaps(&w, 6, &[(2, 3)]); w }], &settings(), None, &Cache::disabled(), &mut |_| {}).unwrap();
        let meta = Metadata { template: DEFAULT_TEMPLATE.into(), cover: Cover::File(dir.join("nope.png")), ..Default::default() };
        let err = prepare_split(&a.files, &a.findings.to_breaks(), &meta).unwrap_err();
        assert!(err.contains("nope.png"), "{}", err);
    }
```

- [ ] **Step 5: Run to verify**

Run: `cargo test`
Expected: everything passes, including the four `write_segment` tests, the six new pipeline tests and the previously existing suite (`split_file_handles_m4b_with_cover_art`, the duration/boundary tests).

If a duration assertion in the new tests fails by more than the tolerance (the input-side seek moved a cut), apply the fallback from the spec: keep `Art::Attach` cutting exactly like `Art::Strip` (output-side seek, no cover) and add a second remux step in `write_segment`, then re-run:

```rust
// ffmpeg -i <cut> -i <cover> -map 0:a -map 1:v -c copy -map_metadata 0 -disposition:v:0 attached_pic -y <cut>.tmp, then rename over the cut.
```

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/pipeline.rs src/cli/split.rs src/gui.rs
git commit -m "Attach album artwork to every written file"
```

---

### Task 4: CLI flag, editor prompt and analyze summary

**Files:**
- Modify: `src/cli/mod.rs`, `src/cli/edit.rs`, `src/cli/analyze.rs`
- Modify: `tests/common/mod.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `meta::Cover`, `cover::{find_embedded, validate}`, `pipeline::file_name`, `user_path`.
- Produces: `--cover <keep|none|PATH>` on `Tags` (so `split` and `run`); `edit::set_cover(plan: &mut Plan, cover: Cover) -> Result<(), String>`.

- [ ] **Step 1: Write the failing unit tests**

Add to the bottom of `src/cli/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use audiobook_splitter_rust::meta::Cover;

    fn tags(cover: Option<&str>) -> Tags {
        Tags { album: None, artist: None, album_artist: None, genre: None, year: None, template: None, cover: cover.map(String::from) }
    }

    #[test]
    fn the_cover_flag_maps_onto_the_metadata() {
        let mut meta = Metadata::default();
        tags(Some("none")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::None);
        tags(Some("/art/front.png")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::File("/art/front.png".into()));
        tags(Some("'/art/with space.jpg'")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::File("/art/with space.jpg".into()), "quotes from pasted paths are stripped");
        tags(Some("keep")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::Keep);
        tags(None).apply(&mut meta);
        assert_eq!(meta.cover, Cover::Keep, "no flag leaves the plan's choice alone");
    }
}
```

Add to `mod tests` in `src/cli/edit.rs` (add `Cover` to the `use audiobook_splitter_rust::meta::...` import at the top of the file first, see Step 3):

```rust
    #[test]
    fn set_cover_accepts_images_and_refuses_anything_else() {
        let dir = std::env::temp_dir().join(format!("abs-edit-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, text) = (dir.join("c.png"), dir.join("c.txt"));
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]).unwrap();
        std::fs::write(&text, "hi").unwrap();
        let mut p = plan();
        set_cover(&mut p, Cover::File(png.clone())).unwrap();
        assert_eq!(p.metadata.cover, Cover::File(png));
        assert!(set_cover(&mut p, Cover::File(text)).is_err());
        assert!(matches!(p.metadata.cover, Cover::File(_)), "a refused image leaves the old choice");
        set_cover(&mut p, Cover::None).unwrap();
        assert_eq!(p.metadata.cover, Cover::None);
    }
```

- [ ] **Step 2: Write the failing end-to-end tests**

Append to `tests/common/mod.rs`:

```rust

// A 64x64 picture of one colour ("red", "green", "blue"); the extension picks JPEG or PNG.
pub fn image(path: &Path, color: &str) {
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c={}:s=64x64", color), "-frames:v", "1", "-y"])
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

// A 15 s MP3 with two silences, carrying `cover` if given.
pub fn mp3_with_gaps(path: &Path, cover: Option<&Path>) {
    let wav = path.with_extension("wav");
    tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"]).arg(&wav);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover).args(["-map", "0:a", "-map", "1:v", "-c:v", "copy", "-disposition:v:0", "attached_pic"]);
    }
    assert!(cmd.args(["-c:a", "libmp3lame", "-y"]).arg(path).status().unwrap().success());
}

// The dominant colour of the first picture in `path`, or None if it has no picture.
pub fn cover_color(path: &Path) -> Option<&'static str> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf", "scale=1:1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    let pixel = out.stdout.get(..3)?;
    let strongest = pixel.iter().enumerate().max_by_key(|(_, v)| **v)?.0;
    Some(["red", "green", "blue"][strongest])
}
```

Append to `tests/cli.rs`:

```rust
// A 15 s MP3 book with a red cover, analyzed into `dir/plan.json`; also `dir/blue.png`.
fn covered(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = common::temp_dir(name);
    let red = dir.join("red.jpg");
    common::image(&red, "red");
    common::image(&dir.join("blue.png"), "blue");
    let book = dir.join("book.mp3");
    common::mp3_with_gaps(&book, Some(&red));
    let plan = dir.join("plan.json");
    bin().arg("analyze").arg(&book).args(["--min-silence", "1", "-o"]).arg(&plan).assert().success();
    (dir, plan)
}

fn colors(out: &std::path::Path) -> Vec<Option<&'static str>> {
    names(out).iter().map(|n| common::cover_color(&out.join(n))).collect()
}

#[test]
fn analyze_reports_the_embedded_cover() {
    let dir = common::temp_dir("cover-analyze");
    let red = dir.join("red.jpg");
    common::image(&red, "red");
    let book = dir.join("book.mp3");
    common::mp3_with_gaps(&book, Some(&red));
    bin().arg("analyze").arg(&book).args(["--min-silence", "1", "-o"]).arg(dir.join("p.json")).assert().success().stdout(contains("Cover: embedded in book.mp3"));
    let plain = dir.join("plain.mp3");
    common::mp3_with_gaps(&plain, None);
    bin().arg("analyze").arg(&plain).args(["--min-silence", "1", "-o"]).arg(dir.join("q.json")).assert().success().stdout(contains("Cover: none embedded"));
}

#[test]
fn split_keeps_the_source_cover_by_default() {
    let (dir, plan) = covered("cover-keep");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).assert().success();
    assert_eq!(colors(&out), [Some("red"), Some("red"), Some("red")]);
}

#[test]
fn split_cover_flag_attaches_an_image_to_every_file() {
    let (dir, plan) = covered("cover-image");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).arg("--cover").arg(dir.join("blue.png")).assert().success();
    assert_eq!(colors(&out), [Some("blue"), Some("blue"), Some("blue")]);
}

#[test]
fn split_cover_none_writes_files_without_artwork() {
    let (dir, plan) = covered("cover-none");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).args(["--cover", "none"]).assert().success();
    assert_eq!(colors(&out), [None, None, None]);
}

#[test]
fn split_rejects_a_bad_cover_before_writing_anything() {
    let (dir, plan) = covered("cover-bad");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).args(["--cover", "missing.png"]).assert().failure().stderr(contains("missing.png"));
    assert!(!out.exists(), "nothing is created when the cover is bad");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test`
Expected: compile errors (`no field 'cover' on type 'Tags'`, `cannot find function 'set_cover'`).

- [ ] **Step 4: Implement**

`src/cli/mod.rs`: add `use audiobook_splitter_rust::meta::{Cover, Metadata};` (replacing the existing `Metadata` import) and `use audiobook_splitter_rust::user_path;`. Add to `Tags` after `template`:

```rust
    /// Album artwork: `keep` (the source's own cover, default), `none`, or a JPEG/PNG path
    #[arg(long, value_name = "keep|none|PATH")]
    pub cover: Option<String>,
```

and in `Tags::apply`, after the `set(&mut meta.template, ...)` line:

```rust
        match self.cover.as_deref() {
            Some("keep") => meta.cover = Cover::Keep,
            Some("none") => meta.cover = Cover::None,
            Some(path) => meta.cover = Cover::File(user_path(path)),
            None => {}
        }
```

(A file literally named `keep` or `none` is addressed as `./keep`.)

`src/cli/edit.rs`: change the imports to

```rust
use audiobook_splitter_rust::cover::{find_embedded, validate};
use audiobook_splitter_rust::meta::{Cover, Metadata};
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::{Plan, PlanBreak};
use audiobook_splitter_rust::user_path;
```

Add the plain edit next to the other plain edits near the top of the file (above `fn heard`):

```rust
// Sets the cover choice, refusing an image that isn't a readable JPEG or PNG.
pub fn set_cover(plan: &mut Plan, cover: Cover) -> Result<(), String> {
    validate(&cover)?;
    plan.metadata.cover = cover;
    Ok(())
}
```

Replace `tags_menu` with:

```rust
fn tags_menu(plan: &mut Plan) -> Result<(), String> {
    println!("Leave a tag empty to keep the source file's own tag.");
    let m = &mut plan.metadata;
    m.album = ask("Album", &m.album, true)?;
    m.artist = ask("Artist", &m.artist, true)?;
    m.album_artist = ask("Album artist", &m.album_artist, true)?;
    m.genre = ask("Genre", &m.genre, true)?;
    m.year = ask("Year", &m.year, true)?;
    m.template = ask("File naming: {n} (from 00), {title}, {album}", &m.template, false)?;
    report(cover_menu(plan));
    Ok(())
}

fn cover_menu(plan: &mut Plan) -> Result<(), String> {
    let embedded = find_embedded(&plan.files).map(|(i, _)| file_name(&plan.files[i].path));
    let items = vec![
        embedded.map_or("Keep the embedded cover (none found in the source files)".to_string(), |name| format!("Keep the cover embedded in {}", name)),
        "No cover art".to_string(),
        "Attach a JPEG or PNG image".to_string(),
    ];
    let current = match &plan.metadata.cover {
        Cover::File(path) => path.display().to_string(),
        _ => String::new(),
    };
    match pick("Cover art", &items)? {
        Some(0) => set_cover(plan, Cover::Keep),
        Some(1) => set_cover(plan, Cover::None),
        Some(2) => {
            let typed = ask("Image file", &current, false)?;
            set_cover(plan, Cover::File(user_path(&typed)))
        }
        _ => Ok(()), // Esc: leave the choice as it was.
    }
}
```

`src/cli/analyze.rs`: add `use audiobook_splitter_rust::cover::find_embedded;`, change the pipeline import to `use audiobook_splitter_rust::pipeline::{analyze, default_metadata, file_name, open_epub};`, and in `summary`, directly after `println!("\n{} chapter(s).", f.breaks.len());`:

```rust
    match find_embedded(&plan.files) {
        Some((i, _)) => println!("Cover: embedded in {}.", file_name(&plan.files[i].path)),
        None => println!("Cover: none embedded in the source file(s); attach one with --cover."),
    }
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test`
Expected: all pass, including the 5 new CLI tests, `the_cover_flag_maps_onto_the_metadata` and `set_cover_accepts_images_and_refuses_anything_else`.

- [ ] **Step 6: Commit**

```bash
git add src/cli tests
git commit -m "Add --cover, the editor's cover prompt and the analyze cover line"
```

---

### Task 5: GUI

**Files:**
- Modify: `src/gui.rs`, `src/ui.html`

**Interfaces:**
- Consumes: `cover::{find_embedded, sniff}`, `pipeline::file_name`.
- Produces: `POST /browse {kind: "image"}` returns `{path, thumb}` (`thumb` is a `data:` URL or null); `/status` carries `cover_source: string | null`; `POST /split` accepts `meta.cover` as `"keep"`, `"none"` or `{"file": path}`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/gui.rs`:

```rust
    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, want) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(input.as_bytes()), want, "{:?}", input);
        }
    }

    #[test]
    fn thumbnail_is_a_data_url_for_images_and_an_error_for_anything_else() {
        let dir = std::env::temp_dir().join(format!("abs-gui-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, text) = (dir.join("c.png"), dir.join("c.txt"));
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0]).unwrap();
        std::fs::write(&text, "not an image").unwrap();
        let url = thumbnail(&png).unwrap().unwrap();
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"), "{}", url);
        assert!(thumbnail(&text).is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib gui::`
Expected: compile error, `cannot find function 'base64'`.

- [ ] **Step 3: Implement the Rust side**

In `src/gui.rs`:

1. Imports: add `use audiobook_splitter_rust::cover;` (next to the other `audiobook_splitter_rust` imports).
2. `Status`: add the field after `meta`:

```rust
    cover_source: Option<String>, // Name of the first file with an embedded cover, for the page's "Keep" option.
```

3. In the analysis thread of `start_analyze`, inside `Ok(a) => {`, before `s.analyzed = a.files;`:

```rust
                s.cover_source = cover::find_embedded(&a.files).map(|(i, _)| file_name(&a.files[i].path));
```

4. `pick`: add the arm before `_`:

```rust
        "image" => dialog.add_filter("Image", &["jpg", "jpeg", "png"]).pick_file(),
```

5. Replace `browse`:

```rust
fn browse(req: BrowseReq) -> Result<Value, String> {
    let output = std::env::current_exe()
        .and_then(|exe| Command::new(exe).args(["--pick", &req.kind]).output())
        .map_err(|e| format!("Failed to open the file dialog: {}", e))?;
    let path = String::from_utf8_lossy(&output.stdout).into_owned();
    let path = Some(path).filter(|p| !p.is_empty());
    // An image is checked at pick time so a bad file is refused straight away.
    let thumb = match (req.kind.as_str(), &path) {
        ("image", Some(p)) => thumbnail(std::path::Path::new(p))?,
        _ => None,
    };
    Ok(json!({ "path": path, "thumb": thumb }))
}

// A `data:` URL for the page to show a picked image, or None if the file is too big to inline.
// Errors if it isn't a JPEG or PNG.
fn thumbnail(path: &std::path::Path) -> Result<Option<String>, String> {
    let mime = if cover::sniff(path)? == "png" { "image/png" } else { "image/jpeg" };
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 8_000_000 {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Can't read '{}': {}", path.display(), e))?;
    Ok(Some(format!("data:{};base64,{}", mime, base64(&bytes))))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib gui::`
Expected: both new tests pass, existing GUI tests still pass.

- [ ] **Step 5: Add the page controls in `src/ui.html`**

CSS: after the `.hint { ... }` rule add

```css
  .choice { display: flex; align-items: center; gap: 8px; font-weight: 400; margin-bottom: 6px; }
  .choice input { margin: 0; }
  #cover-thumb { display: block; width: 96px; height: 96px; object-fit: cover; border-radius: 7px; border: 1px solid var(--border); margin-top: 8px; }
```

HTML: replace

```html
      <div id="name-preview" class="preview"></div>
    </div>
  </section>
```

with

```html
      <div id="name-preview" class="preview"></div>
    </div>
    <div class="field">
      <label id="cover-label">Cover art</label>
      <div role="radiogroup" aria-labelledby="cover-label">
        <label class="choice"><input type="radio" name="cover" value="keep" checked>Keep the embedded cover <span id="cover-source" class="hint"></span></label>
        <label class="choice"><input type="radio" name="cover" value="none">No cover art</label>
        <label class="choice"><input type="radio" name="cover" value="file">Use an image
          <button type="button" id="cover-pick"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="m21 15-5-5L5 21"/></svg>Choose image</button>
        </label>
      </div>
      <img id="cover-thumb" alt="Chosen cover art" hidden>
      <div id="cover-path" class="hint"></div>
      <div class="hint">JPEG or PNG. Written to MP3 and M4A/M4B files.</div>
    </div>
  </section>
```

JS, in three places:

1. Directly before the line `// One shared player; ...`:

```js
let coverFile = null; // Path picked with "Choose image".
const coverChoice = () => {
  const v = document.querySelector('input[name=cover]:checked').value;
  return v === 'file' ? { file: coverFile } : v;
};
$('cover-pick').addEventListener('click', async (e) => {
  e.preventDefault(); // The button sits inside a label; don't also toggle the radio twice.
  showError(null, 'split-error');
  try {
    const { path, thumb } = await api('/browse', { kind: 'image' });
    if (!path) return;
    coverFile = path;
    $('cover-path').textContent = path;
    $('cover-thumb').hidden = !thumb;
    if (thumb) $('cover-thumb').src = thumb;
    document.querySelector('input[name=cover][value=file]').checked = true;
  } catch (err) { showError(err, 'split-error'); }
});
```

2. In the split handler, replace

```js
  const meta = { template: $('template').value };
  for (const f of metaFields) meta[f] = $('m_' + f).value;
```

with

```js
  const meta = { template: $('template').value, cover: coverChoice() };
  for (const f of metaFields) meta[f] = $('m_' + f).value;
  if (meta.cover.file === null) { showError(new Error('Choose an image file for the cover first.'), 'split-error'); return; }
```

3. In `loadEditor`, after `for (const f of metaFields) $('m_' + f).value = s.meta[f];`:

```js
  $('cover-source').textContent = s.cover_source ? '(' + s.cover_source + ')' : '(none found)';
```

- [ ] **Step 6: Verify the page in a browser**

```bash
mkdir -p /tmp/abs-gui && cd /tmp/abs-gui
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=red:s=64x64 -frames:v 1 -y red.jpg
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=blue:s=64x64 -frames:v 1 -y blue.png
ffmpeg -hide_banner -loglevel error -f lavfi -i "aevalsrc=if(between(t\,4\,6)\,0\,sin(2*PI*440*t)):d=12" -i red.jpg -map 0:a -map 1:v -c:v copy -disposition:v:0 attached_pic -c:a libmp3lame -y book.mp3
cd - && cargo run -- gui
```

In the page: analyze `/tmp/abs-gui/book.mp3` and confirm the "Keep the embedded cover (book.mp3)" label. Choose `blue.png` through the picker and confirm the thumbnail appears and the radio flips to "Use an image". Split to `/tmp/abs-gui/out` and check:

```bash
for f in /tmp/abs-gui/out/*.mp3; do ffprobe -v error -show_entries stream=codec_name,codec_type -of compact "$f"; done
```

Expected: each file lists an `mjpeg` or `png` video stream. Then repeat with "No cover art" and expect audio only. Try "Use an image" with no file chosen and expect the inline error "Choose an image file for the cover first."

- [ ] **Step 7: Commit**

```bash
git add src/gui.rs src/ui.html
git commit -m "Add cover art controls to the GUI"
```

---

### Task 6: Docs and full verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the README**

At the end of the "Metadata Editor (GUI)" bullet (line 23) append: ` Cover art: keep the cover already embedded in the source (the default), attach a JPEG or PNG, or write none. It is written to every MP3 and M4A/M4B output.`

In the `split <plan> -o <dir>` row of the CLI table (line 156) change `(`--album`, `--artist`, `--template`, ...)` to `(`--album`, `--artist`, `--template`, `--cover keep|none|PATH`, ...)`.

- [ ] **Step 2: Full verification**

Run each and confirm the stated result:

```bash
cargo test 2>&1 | grep -E "^test result|FAILED|panicked"
```
Expected: every `test result` line is `ok`, 0 failed.

```bash
cargo test -- --ignored 2>&1 | grep -E "^test result"
```
Expected: `ok` (the two Whisper tests; unchanged by this work).

```bash
cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c
```
Expected: no `error`, and no warning that mentions `src/cover.rs`. Compare the count against the 17 style warnings that existed before this work (see `git stash`-free check: `cargo clippy --all-targets 2>&1 | grep -B2 -A6 "cover.rs"` prints nothing).

```bash
git status --short
```
Expected: only the untracked `docs/superpowers/specs/2026-11-14-tui-editor-design.md` remains.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document album artwork"
```

---

## Self-Review (against the spec)

- **Model, serde, back-compat:** Task 1.
- **`cover.rs` (`sniff`, `find_embedded`, `resolve`, `Resolved` with Drop):** Task 2.
- **`write_tracks`/`write_segment` take a cover; `-vn` gone from the artwork path; input-side seek; concat adds the image input; `split_file` unchanged; other containers skipped with a log:** Task 3.
- **Validation in `prepare_split`; `Keep` extraction failure is a log line:** Tasks 2 and 3.
- **CLI `--cover`, editor prompt with status, `analyze` note:** Task 4.
- **GUI row, picker, thumbnail, `cover_source`, no endpoint serving arbitrary paths:** Task 5.
- **Tests listed in the spec:** sniff, serde round trip and missing-field default, flag parsing (Tasks 1, 2, 4); per-format keep/replace/none/no-embedded and the concat case (Task 3); CLI end to end (Task 4); the existing M4B-with-cover and boundary tests run unchanged in Task 3 Step 5.
- **Risk fallback (input-side `-ss` accuracy):** spelled out in Task 3 Step 5.
- **Names consistent across tasks:** `Cover`, `cover::{supported, sniff, validate, find_embedded, resolve, Resolved}`, `Art::{Untouched, Strip, Attach}`, `write_segment(.., cover: Option<&Path>)`, `write_tracks(.., files, cover, emit)`, `set_cover`, `cover_source`.
