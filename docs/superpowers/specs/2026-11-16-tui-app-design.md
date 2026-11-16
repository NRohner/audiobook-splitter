# Terminal app (`tui`)

Date: 2026-11-16
Branch: `feat/cli-updates` (builds on the album artwork work and the shared `pipeline`)
Supersedes the scope of `2026-11-14-tui-editor-design.md`, which becomes slice 1 below and stays the detailed source for the chapter editor's keys, `App` state and tests.

## Goal

`cargo run -- tui` opens one full-screen terminal app that does everything the web GUI does: choose a book, set the analysis options, watch the analysis, edit chapters, set tags and cover art, split, and see the result. It works over SSH and in tmux. The existing commands keep working, and `edit` and `run` use the same full-screen chapter editor.

## Decisions

- **One app, one `Screen` state machine** (not a chain of separate mini-apps), so the user can move back and forth (editor to setup, editor to tags) without losing state.
- **Folded-in editor:** the full-screen chapter editor from the 2026-11-14 spec is a screen of the app. `edit` and `run` open it by default and fall back to the current prompt menu with `--simple`, on a dumb or too-small terminal, or if terminal setup fails.
- **Built-in file browser** for the input, EPUB, output folder and cover image; typed or pasted paths also work.
- **One new dependency: `ratatui`** (re-exports its `crossterm` backend). Verify the current version and API when writing the plan. The file browser and text input are hand-written on `std::fs`.
- **Three slices**, each its own plan and usable on its own (below).
- Not included: mouse support, in-terminal audio playback, cancelling a running analysis (the GUI cannot either), a rescan key (use the `rescan` command), themes.

## Flow

```
Setup --> Analyze (progress) --> Chapter editor --> Split (progress) --> Done
  ^                                  |   ^
  |__________________________________|   |
                                    Tags and cover (a form opened from the editor)
```

- `tui` with no argument starts at Setup.
- `tui <audio file or folder>` starts at Setup with the input filled in.
- `tui <plan.json>` opens that plan in the editor. Because a plan has no output folder, saving or splitting asks for one first (the file browser in folder mode).
- `s` in the editor goes to Split (asks for the output folder if unset). Back from the editor returns to Setup with the previous values kept; changing the input or settings and re-analyzing replaces the plan after a confirmation if there are unsaved edits.
- Done shows the files written (count and folder, any failures) and offers: quit, or start another book.

## Architecture

```
src/cli/tui/mod.rs        Args, run, App { screen, plan, settings, ... }, Screen enum, event loop
src/cli/tui/terminal.rs   TerminalGuard (raw mode, alternate screen, panic hook, restore on Drop), use_tui()
src/cli/tui/input.rs      single-line text input (insert, backspace, delete, cursor, unicode)
src/cli/tui/browser.rs    file browser widget            (slice 2)
src/cli/tui/setup.rs      Setup screen                   (slice 2)
src/cli/tui/progress.rs   Analyze and Split screens      (slice 2)
src/cli/tui/editor.rs     chapter editor screen          (slice 1)
src/cli/tui/tags.rs       tags, naming and cover form    (slice 1, browser hookup in slice 3)
src/cli/edit/{mod,ops,menu}.rs   as in the 2026-11-14 spec: pure edit functions in ops.rs, the dialoguer menu in menu.rs
src/cli/preview.rs        exposes write_clip(plan, n, out) used by `preview` and the editor's preview key
```

- Every screen is a plain struct with `handle_key(&mut self, key) -> Outcome` and `render(&self, frame)`, with no terminal I/O, so tests drive them with synthetic keys and ratatui's `TestBackend`. Only `run_loop` touches crossterm.
- `Outcome` tells the app what to do next: stay, switch screen, start a job, save, quit.
- **Long jobs on a worker thread.** Analyze and Split call `pipeline::analyze` and `pipeline::write_tracks` (which already report `Event::{Stage, Tick, Log}` through a callback) from a thread, sending events over an `mpsc` channel. The app drains the channel each frame (poll with a ~100 ms timeout) and draws a gauge plus a scrolling log. The thread's final result (`Analysis` or `SplitReport`) arrives as a last channel message.
- **Fail fast before the thread starts**, as the GUI does: validate settings (`Settings::validate`), open the EPUB (`pipeline::open_epub`), check the input exists and the output folder is usable, check `ffmpeg`/`ffprobe` (`check_tools`). Errors show on the Setup screen.
- Analysis builds the plan exactly as `cli::analyze::build_plan` does (`Plan::new(files, settings, epub path, findings, metadata)` with `default_metadata`), so a plan made in the TUI is identical to one from `analyze` and can be saved as `plan.json`.
- Quitting while a job runs asks for confirmation; confirming ends the process (jobs are not cancellable), after restoring the terminal.

### Terminal safety and fallback

`TerminalGuard` enters raw mode and the alternate screen and restores both in `Drop`; a panic hook restores the terminal before the previous hook prints, so a crash never leaves an SSH session unusable. Ctrl-C is a key (quit prompt), not a signal that skips the guard.

`use_tui(simple, term, size, is_tty) -> bool` (pure): false if `simple`, not a TTY, `TERM` unset, empty or `dumb`, or size below 60x15. For `edit` and `run` a false result means the prompt menu, with a one-line note when setup itself failed. `tui` itself has no menu fallback: it prints why it cannot run and suggests `run` or the individual commands, and exits with an error.

## Slice 1: TUI foundation and chapter editor

Replaces the `edit` and `run` prompt menu. Everything in `2026-11-14-tui-editor-design.md` applies (layout, key table, `App` with undo/redo snapshots, `apply()`, quit prompt, refusals in the status line, rendering at 80x24 and 60x15), with these changes:

- The editor is the `Screen::Editor` of the shared `App`, so slice 2 reuses it unchanged. In slice 1 a minimal `App` hosts only this screen.
- The tags form (`t`) also edits cover art: three choices (keep embedded, none, an image) with a typed path field for the image, validated by `cover::validate` and applied with `edit::set_cover`; it shows "embedded in X" or "none found" using `cover::find_embedded`. Slice 3 adds the browser to the image field.
- `edit` and `run` take `--simple`. `edit_plan(plan, simple)` chooses the editor.
- `preview::write_clip` is extracted, `edit.rs` becomes the `edit/` folder, and `ops.rs` moves unchanged apart from visibility.

Deliverable: `edit` and `run` open the full-screen editor; `--simple` and unsuitable terminals get today's menu; all current edit tests still pass.

## Slice 2: the `tui` command

Adds `tui [PATH]`, the Setup screen, the file browser, and the Analyze and Split progress screens, wired into the flow above. At the end of slice 2 a user can run the whole workflow with default metadata plus the slice 1 tags form.

**Setup screen** (a form; Tab and Shift-Tab move, Enter on a path field opens the browser, Ctrl-A starts the analysis):

| Field | Type | Default |
|---|---|---|
| Audio file or folder | path (browser: files and folders) | from `PATH` |
| EPUB (optional) | path (browser: `.epub`) | empty |
| Output folder | path (browser: folders) | empty |
| Minimum silence (s) | number | 2.0 |
| Noise level (dB) | number | -40.0 |
| Match threshold | number 0-1 | 0.4 |
| Use cache | toggle | on |

Each path field shows live feedback ("3 audio files", "not found", "EPUB: 28 chapters" once opened). Defaults match the `analyze` flags.

**File browser** (popup): lists the directory (folders first, then files matching the filter), shows the current path, and supports Up/Down/`j`/`k`, PgUp/PgDn, Home/End, Enter (open a folder or pick a file), Backspace or `h` (parent), `~` (home), `/` then typing to filter the list, `.` to toggle hidden entries, Tab to switch to typing a full path, and in folder mode `Space` to choose the current folder. Esc closes it. It starts at the field's current value, else the last browsed folder, else the home folder. On Windows it starts in the user profile and moves by parent folder; another drive is reached by typing its path.

**Progress screens:** a gauge for the current stage (`Event::Stage` name and total, `Event::Tick`), elapsed time, and a scrolling log of `Event::Log` lines. Analysis errors for individual files are logged; if no file could be read, the app returns to Setup with the message.

## Slice 3: cover browser hookup and polish

- The tags form's image field gets "Choose image" using the file browser filtered to JPEG and PNG.
- Back navigation between Editor, Setup and Tags with state preserved; the Done screen's "another book".
- Terminal-resize handling on every screen, a help popup (`?`) on every screen, and the key-help footer text reviewed per screen.

## Errors and edge cases

- Setup validation errors (bad number, missing input, unreadable EPUB, missing ffmpeg) show under the field or in a status line and never leave the screen.
- A job thread panic is reported as an error on the progress screen; the terminal is still restored.
- A plan whose audio files have moved fails `plan.check_files()` on open, with the existing message.
- Terminal resized below 60x15 mid-session: keep going with a degraded layout (as in the editor spec); the app never exits for a resize.
- Output folder exists but is a file, or cannot be created: reported on Setup before the split starts.
- Splitting uses `prepare_split`, so a bad cover or empty naming template fails before anything is written; the error returns the user to the editor or tags form.

## Testing (tests first)

- Pure units: `use_tui` (each condition), the `Input` widget (insert, backspace, delete, cursor, unicode), browser listing and filtering on a temp directory tree (folder ordering, extension filter, hidden toggle, parent, type-to-filter, path typing), number-field parsing and range errors.
- Screen state machines through `handle_key` with synthetic keys: editor behaviour as listed in the editor spec; Setup field navigation, validation and the transition to Analyze; progress screens fed a synthetic `Event` stream and final result; the flow Setup, Analyze, Editor, Split, Done, plus back navigation and the unsaved-changes confirmation.
- Rendering with `TestBackend` at 80x24 and 60x15 for each screen and popup, with no panic at the minimum size or with long titles and paths.
- A worker-thread test: run `analyze` on a generated tone file through the real channel plumbing and assert the app reaches the editor with the expected chapters.
- Existing tests keep passing: `edit` still needs a terminal when stdin is piped; `edit --simple`, `run --simple` and `tui --help` parse.
- Manual, in a real terminal (a subagent cannot drive one): run `tui` over `ssh localhost` inside tmux, go through every screen and key once, resize the window, quit mid-job and with unsaved edits, and confirm the terminal is restored after `q`, Ctrl-C and a forced panic.

## Risks

- **Size.** This is the largest feature in the project so far; the three slices keep each deliverable usable and reviewable, and slice 1 alone already replaces the weakest part of the terminal experience.
- **ratatui API drift.** The version and widget APIs are checked when each plan is written.
- **Windows terminals.** crossterm supports them; the manual test should be repeated on Windows before claiming support there.
- **Progress over a slow terminal.** Draw at most every 100 ms regardless of how many events arrive.

## Out of scope

Mouse support, audio playback inside the TUI, cancelling jobs, rescanning from the TUI, themes and colour configuration, multi-select and bulk edits, non-interactive scripting of the TUI.
