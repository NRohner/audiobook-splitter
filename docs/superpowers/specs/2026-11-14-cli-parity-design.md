# CLI parity with the GUI (part A)

Date: 2026-11-14. Branch: `feat/cli-updates`.

## Goal

Make the CLI do everything the GUI does, so the app is fully usable over SSH on a headless server. Part B (polish: TUI chapter editor, `doctor`, completions, docs rewrite) is a separate spec and is out of scope here.

## Decisions

- **Shape:** subcommands with flags (clap), scriptable, with interactive prompts only where a value is missing and stdin is a terminal. Not a full-screen TUI in part A.
- **Workflow:** staged commands joined by a saved plan file, so a long Whisper run is done once and can be edited and split later. `run` chains the stages for the one-shot case.
- **Preview:** the editor shows what Whisper heard around each break; `preview` writes a clip file and plays it with `ffplay` only when one is available.
- **No arguments:** print help. The GUI needs `gui`. `--cli` stays as a hidden alias for `run`.
- **Architecture:** move the analyze, EPUB match, re-scan and split orchestration out of `gui.rs` into a shared `pipeline` module in the lib. The GUI and CLI both call it, so parity holds by construction.

## Architecture

```
src/lib.rs, chapters.rs, meta.rs, ...   existing building blocks (unchanged)
src/pipeline.rs   NEW  analyze / match / rescan / split, progress via callback
src/plan.rs       NEW  Plan type, JSON load/save, validation
src/gui.rs        now a thin HTTP layer over pipeline (Status stays GUI-only)
src/cli/mod.rs    NEW  clap definitions and dispatch
src/cli/*.rs      NEW  one file per subcommand, plus shared progress/prompt helpers
```

### `pipeline`

Free functions with no GUI state. Progress is reported through a `&dyn Fn(Progress)` callback, where `Progress` is `{ stage: String, done: usize, total: usize }`; the log lines the GUI shows are returned or reported the same way.

- `analyze(inputs, settings, progress) -> Analysis`: silence detection per file, then, with an EPUB, Whisper transcription and `pick_breaks`. Returns audio files, breaks (with scores and before/after text), every candidate silence with its transcript, missing chapters, and suggested metadata.
- `rescan(analysis, chapters, progress)`: the logic of `rescan_one`, including the looser silence and noise factors now living in `gui.rs`.
- `split(plan, output_dir, progress) -> Vec<PathBuf>`: `plan_from_breaks`, `tracks`, then `write_segment` per track.

The GUI's `start_analyze`, `start_rescan` and `start_split` keep their request validation and thread spawning, and map the callback onto `Status`. Behaviour must not change; the existing GUI tests keep passing.

### `plan`

A versioned JSON file (`"version": 1`):

```
{ version, created, settings: { min_silence, noise_db, threshold, epub, model },
  files:     [{ path, duration, silences: [{start, end}] }],
  breaks:    [{ file, at, title, score? }],
  silences:  [{ file, at, before, after }],   // every candidate, for the editor
  missing:   [{ chapter, title, tried }],
  metadata:  { album, artist, album_artist, genre, year, template } }
```

- Paths are stored absolute. On load, each file must exist and have the recorded duration (within 0.5 s) or the command fails with the name of the changed file.
- A `version` newer than the binary supports is a clear error.
- `chapters` (the EPUB text) is not stored; re-scan reloads it from `settings.epub`.
- Types that need serde (`Silence`, `ChapterBreak`, `Break` already has it) get derives; no format changes to existing code.

## Commands

| Command | Behaviour |
|---|---|
| `analyze <input> [-o plan.json]` | `<input>` is a file or folder. Flags: `--epub`, `--min-silence` (default 2.0), `--noise-db` (default -40), `--threshold` (default 0.4). Progress bars per stage; prints a summary table (breaks, missing chapters). Writes the plan (default `plan.json`). |
| `rescan <plan> [--chapter N ...]` | Re-scans the missing chapters (all, or those given), updates the plan in place. Requires an EPUB analysis. |
| `edit <plan>` | Interactive menu loop (dialoguer): list breaks with score and heard text; rename, remove, add at `h:mm:ss`; move a break to the previous or next silence; promote a skipped silence; edit metadata and the naming template. Saves on exit. Fails with a hint if stdin is not a terminal. |
| `preview <plan> <n> [-o clip.mp3]` | Writes 30 s from break *n* (1-based in the listing) to a file; plays it with `ffplay` if on `PATH` and `--play` is given or a local audio device is detected. |
| `split <plan> -o <dir>` | Writes tagged tracks. Metadata flags (`--album`, `--artist`, `--album-artist`, `--genre`, `--year`, `--template`) override the plan. Progress bar over tracks; prints the files written. |
| `run <input> -o <dir>` | `analyze`, then `edit` (skipped with `--yes` or when not a terminal), then `split`. Takes all `analyze` and `split` flags. Writes the plan next to the output unless `--plan` is given. |
| `gui` | Starts the web GUI as today. |

Without an EPUB, `analyze` produces silence-based breaks exactly as the GUI does; `edit` and `split` work the same way.

## Output and errors

- Progress bars use `indicatif`; in part A they are plain bars and spinners. Colour and TTY detection beyond what `indicatif` and `console` do by default belong to part B.
- Fail fast, before any slow work: missing `ffmpeg`/`ffprobe`, missing Whisper model (message names `WHISPER_MODEL` and the download step), unreadable EPUB, bad flag values (same checks as `start_analyze`).
- Every error is one line plus the fix where there is one, written to stderr. Exit code 0 on success, non-zero otherwise.
- Ctrl-C during analysis leaves no partial plan file (write to a temp file, then rename).

## Testing (tests first)

- `pipeline`: analyze, rescan and split on short generated audio (ffmpeg sine and silence), asserting segment counts and file names; EPUB paths use the existing fixtures in `src/` tests where possible.
- `plan`: save/load round trip, changed-file rejection, newer-version rejection.
- `cli`: integration tests with `assert_cmd` for `analyze` then `split`, `--help` output for each command, error exits for a missing file and bad flags. `edit` is tested through its pure functions (rename, remove, move, add), not through the prompt loop.
- GUI regression: existing tests pass unchanged after the refactor.
- Parity checklist run by hand once against the GUI: analyze, EPUB match, re-scan, each editor action, metadata tags, multi-file split.

## New dependencies

`clap` (derive), `indicatif`, `dialoguer`; dev: `assert_cmd`. No new native dependencies.

## Out of scope for part A

TUI editor, `doctor`, shell completions and man page, `--json`/`--quiet`/`--verbose`, config file, resumable transcription, documentation rewrite (the README gets only the minimum edits to stay correct), Linux Vulkan support.

## Risks

- The `gui.rs` refactor is the riskiest step. Do it first, behind the existing tests, before any CLI code.
- `dialoguer` menus over slow SSH links are fine (line-based redraws), but the `edit` flow must also have a non-interactive equivalent for scripting; that is deferred to part B and noted in its spec.
