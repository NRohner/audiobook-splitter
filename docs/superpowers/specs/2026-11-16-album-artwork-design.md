# Album artwork

Date: 2026-11-16
Branch: `feat/cli-updates`

## Goal

Every file written by a split carries album artwork. The user chooses where it comes
from: the cover already embedded in the source file(s) (the default), an image file they
attach, or nothing. The choice is part of the metadata, so it shows up in the CLI flags,
the interactive editor, the GUI and the saved plan.

## Today's behavior (why this isn't a pass-through)

- `cut()` in `src/lib.rs` passes `-vn` for M4B/M4A inputs, so those parts lose their cover
  (`ponytail:` comment at `lib.rs:264`). MP3 parts keep an embedded cover only by accident,
  because `-c copy` copies every stream.
- `concat()` maps `-map 0:a` only, so any chapter joined from several pieces loses its
  cover, in every format.
- Tags travel as `-metadata key=value`, which cannot carry an image.

## Approach

FFmpeg only, no new dependency. Resolve one cover image per split, then add it as an extra
input to every output with `-disposition:v attached_pic`.

Verified with ffmpeg 9.0.2: MP3 (JPEG, `-id3v2_version 3`) and M4B (PNG and JPEG) both
accept an attached picture with `-c copy`, and an embedded cover extracts with
`-map 0:v:0 -frames:v 1 -c copy`. One trap found while testing: an output-side `-ss`
also applies to the one-frame image input and drops its only frame, so the output has no
cover and no error. The seek must therefore be an input option on the audio only.

## Components

### `Cover` (in `src/meta.rs`)

```rust
pub enum Cover { Keep, None, File(PathBuf) }   // Default: Keep
```

`Metadata` gets `pub cover: Cover`. It serializes as `"keep"`, `"none"` or
`{"file": "<path>"}`. `Metadata` already uses `#[serde(default)]`, so plan files saved
before this change load as `Keep`. `Metadata` is compared in `cli/edit.rs::changed`, so
the new field is covered by the existing unsaved-changes check.

### `src/cover.rs` (new, one purpose: turn a `Cover` choice into an image path)

- `sniff(path) -> Result<Kind, String>`: reads the first bytes, accepts JPEG and PNG, and
  rejects anything else with a message naming the file and the accepted types.
- `find_embedded(files: &[AudioFile]) -> Option<usize>`: the index of the first source
  file, in plan order, that has a video stream (any, not only `attached_pic`, since
  Audible-style M4Bs store the cover as a plain MJPEG track; see
  `split_file_handles_m4b_with_cover_art`). Used for the status text in the CLI and GUI.
- `resolve(cover, files, scratch_dir) -> Result<Resolved, String>`:
  - `Keep`: extract from the file found by `find_embedded` into a temp file. If nothing is
    found, or the extracted image is not JPEG/PNG, resolve to "no cover" and report why.
  - `None`: no cover.
  - `File(p)`: `sniff(p)`, then use `p` directly.
- `Resolved` holds `Option<PathBuf>` plus an optional temp file removed on `Drop`, so an
  extracted cover never outlives the split, even on error.

### Writing

- `write_tracks` and `write_segment` take `cover: Option<&Path>`. `write_segment` always
  selects audio explicitly (`-map 0:a`) and, when a cover is given, adds it as the last
  input and maps it with `attached_pic`. An explicit "no cover" therefore strips MP3
  covers too.
- `cut()`: when a cover is attached, `-ss` moves before the audio `-i` (input seek). The
  `-vn` hack is deleted from this path.
- `concat()`: the cover is one more `-i`, mapped alongside `-map 0:a`. It has no `-ss`.
- `split_file` (the older start/end splitter) keeps its current behavior.
- Containers other than MP3 and M4A/M4B: the cover is skipped with a log line, and the
  split continues.

### Validation and errors

- `prepare_split` validates `Cover::File` (exists, readable, JPEG/PNG), so the CLI and the
  GUI both fail before any file is written, like the existing naming-convention check.
- Extraction failure under `Keep` is a log line, not an error: the user still gets their
  audio, without artwork.
- If attaching fails for one output, the existing per-file failure path reports it and
  removes the partial file. No size cap on custom images (YAGNI).

### CLI

- `--cover <keep|none|PATH>` in the shared `Tags` args, so `split` and `run` get it.
  `apply()` maps it onto `Metadata.cover`. A file literally named `keep` or `none` is
  addressed as `./keep`.
- Interactive editor (`tags_menu`): a "Cover art" prompt with the three choices and, for
  `Keep`, a status line such as "Embedded in 01.m4b" or "No embedded cover found".
- `analyze` output notes where an embedded cover was found.

### GUI

- Metadata section gets a "Cover art" row: Keep embedded / None / Custom image.
- "Choose image..." opens the native picker (`rfd`, already a dependency) on the Rust side
  and returns the path plus a data URL for a small thumbnail. There is no endpoint that
  serves arbitrary paths.
- The analysis result carries `cover_source` so the UI can show "Embedded in X" or "No
  embedded cover found" next to the Keep option.
- No emojis; plain SVG or text for the controls, per the user's UI preference.

## Testing (tests first)

Unit:
- `sniff` accepts JPEG and PNG, rejects text, GIF and an empty file.
- `Cover` serde round trip, and a plan JSON without the field loads as `Keep`.
- `--cover` parses `keep`, `none` and a path, and `apply` sets the field.

FFmpeg integration, MP3 and M4B each, using generated inputs:
- Keep: every output has an `attached_pic` stream whose image equals the source's cover.
- Custom PNG: every output carries that PNG, not the source's cover.
- None: no output has a picture stream.
- Keep with no embedded cover: outputs have no picture and the split succeeds.
- A chapter joined from two files (concat path) carries the cover.
- Regression guard: the existing M4B-with-cover and cut-accuracy tests still pass
  unchanged, since the seek moved to the input side.

CLI end to end in `tests/cli.rs`: `split --cover <png>` produces covered files, and
`split --cover missing.png` exits non-zero before writing anything.

## Risks

- **Input-side `-ss` accuracy.** If the existing duration and boundary tests regress with
  the seek on the input, fall back to the current cut followed by a cheap second remux
  pass that adds the cover (`-i cut -i cover -map 0:a -map 1:v -c copy`). It costs one
  extra read and write of each output, and the rest of the design is unchanged.
- **Odd embedded covers** (BMP, WebP, and so on) are skipped with a log line, not converted.

## Out of scope

Per-chapter artwork, resizing or recompressing images, a GUI preview of the embedded cover
(only a text status), and artwork for containers other than MP3 and M4A/M4B.
