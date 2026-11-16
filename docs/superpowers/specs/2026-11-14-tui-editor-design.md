# TUI chapter editor (part B, sub-project 2)

Date: 2026-11-14. Builds on `2026-11-14-cli-parity-design.md` (the menu editor and its pure edit functions) and the cache work already merged on `feat/cli-updates`.

## Goal

Replace the dialoguer menu in `edit` (and the editor step of `run`) with a full-screen terminal editor: a chapter table, a detail pane showing what Whisper heard around the selected break, and single-key actions. It must work over SSH and in tmux, and fall back to the existing menu where a full-screen UI is a bad idea.

## Decisions

- **Default with automatic fallback:** the TUI is the default. The existing menu editor is used instead when `--simple` is passed, when `TERM` is unset or `dumb`, when the terminal is smaller than 60 columns by 15 rows, or when terminal setup fails (with a one-line note).
- **Layout:** chapter table on top, detail pane for the selected chapter underneath, key-help footer.
- **Extras on top of parity:** undo/redo, a missing-chapters view, a preview-clip key, jump to a time or chapter.
- Not included: mouse support, in-TUI audio playback, multi-select.

## Architecture

`src/cli/edit.rs` becomes a folder:

```
src/cli/edit/mod.rs    Args, run, edit_plan(plan, simple) -> Result<bool, String> (chooses TUI or menu)
src/cli/edit/ops.rs    the pure edit functions and their tests, moved unchanged
src/cli/edit/menu.rs   the dialoguer menu, moved unchanged apart from imports
src/cli/edit/tui.rs    new: App state machine, rendering, terminal guard
```

- `ops.rs` keeps `is_fixed`, `add`, `rename`, `remove`, `neighbour`, `move_break`, `unused_silences`, `changed`, `heard`. They gain `pub(super)` visibility where the TUI needs them. Behaviour does not change.
- `edit_plan(plan, simple)` keeps returning `true` to save, `false` to leave without saving; `run` passes its own `--simple` flag through. `edit` and `run` both get `--simple` ("use the plain menu instead of the full-screen editor").
- Dependencies: `ratatui` (0.30, which re-exports its `crossterm` backend). No other new crates. The single-line text input is written by hand (~60 lines: characters, cursor, insert, backspace, delete, left/right/home/end).
- `preview.rs` exposes `pub fn write_clip(plan: &Plan, n: usize, out: Option<PathBuf>) -> Result<String, String>` (cuts the clip, writes it, returns the "Wrote ..." message); both the `preview` command and the TUI call it.

### Choosing the editor

`fn use_tui(simple: bool, term: Option<&str>, size: (u16, u16), is_tty: bool) -> bool` (pure, unit-tested): false if `simple`, if not a TTY, if `term` is None/empty/`dumb`, or if `size` is below 60x15. `edit_plan` reads the real values and calls it. If entering the TUI fails (terminal setup error), print "Full-screen editor unavailable (<reason>); using the plain menu." and run the menu.

### The App

```
struct App { plan: Plan, original: (Vec<PlanBreak>, Metadata), undo: Vec<Snapshot>, redo: Vec<Snapshot>,
             selected: usize, scroll: usize, mode: Mode, status: String }
struct Snapshot { breaks: Vec<PlanBreak>, metadata: Metadata, selected: usize }
enum Mode { Normal, Input(InputKind, Input), SilencePicker { spots, selected, title }, Missing { selected },
            Tags { fields, focus }, ConfirmQuit, Help }
enum Outcome { Continue, Save, Discard }
fn handle_key(&mut self, key: KeyEvent) -> Outcome
fn render(&self, frame: &mut Frame)
```

`handle_key` and `render` use no terminal I/O, so tests drive them with synthetic key events and ratatui's `TestBackend`. The real loop (`run_tui`) is a thin shell: draw, read one crossterm event, `handle_key`, repeat until the outcome is `Save` or `Discard`.

Every edit goes through one `apply(&mut self, change: impl FnOnce(&mut Plan) -> Result<(), String>)` that snapshots first, runs the change, and on error pops the snapshot and shows the message in the status line. Undo and redo swap snapshots and restore the selection. Any new edit clears the redo stack.

### Terminal safety

A guard (`struct TerminalGuard`) enters raw mode and the alternate screen and restores both in `Drop`; a panic hook (installed for the duration of the editor) restores the terminal before the previous hook prints, so a crash never leaves the SSH session unusable. Ctrl-C is handled as a key (quit prompt), not as a signal that skips the guard.

### Screen

Three stacked regions: the chapter table (`#`, where, title, match score; the selected row highlighted; scrolls to keep the selection visible; titles truncated to fit), the detail pane for the selected chapter (position, score, heard-before and heard-after text word-wrapped to the width, previous and next silence times, "no transcript" when none), and a two-line key help footer. A one-line status area sits in the footer for messages and errors. Popups are centred boxes drawn over the table.

### Keys

| Key | Action |
|---|---|
| Up/Down, `k`/`j`, PgUp/PgDn, Home/End | move the selection |
| Enter, `r` | rename (text popup, initial text = current title) |
| `[` / `]` | move the break to the previous / next silence (`ops::move_break`) |
| `d` | remove (undoable; no confirmation) |
| `a` | add a chapter at a time (`h:mm:ss`; asks for the file first when there are several; then a title) |
| `n` | add a chapter at a skipped silence (picker listing time and the start of what was heard; then a title) |
| `m` | missing-chapters list; Enter on one opens the silence picker with that chapter's title preset |
| `t` | tags and file naming form (Tab/Shift-Tab between fields, Enter saves, Esc cancels) |
| `g` | jump to a time (`h:mm:ss`, selects the nearest chapter at or before it) or a chapter number (plain integer) |
| `p` | write a preview clip via `preview::write_clip`; show the path in the status line |
| `u` / Ctrl-r | undo / redo |
| `s` | save and exit |
| `q`, Ctrl-c | quit; if there are unsaved changes (`ops::changed`), a prompt: `s` save, `d` discard, Esc cancel |
| `?` | help popup listing all keys |
| Esc | close a popup |

Refusals (moving or removing the fixed first chapter, no silence to move to, a duplicate position, a blank title) appear in the status line and never end the session. The first chapter's row is marked as fixed.

`g` input: if the text parses as `h:mm:ss` / `m:ss` / seconds (`time::parse_time`) and contains a `:` or a `.`, treat it as a time; a plain integer is a chapter number (1-based).

## Errors and edge cases

- A plan with zero chapters cannot occur (the first chapter always exists); the table still renders defensively.
- Terminal resized below the minimum while editing: keep going; the layout degrades (table first, detail pane shrinks to one line, footer shows only `? help`).
- Very long chapter lists: the table scrolls; selection jumps (PgUp/PgDn, Home/End, `g`) keep it visible.
- `p` when the audio file is gone: the error from `write_clip` goes to the status line.
- Reading input fails (EOF or closed terminal): leave the editor without saving and return the error.

## Testing (tests first)

- `use_tui`: each condition (simple, not a TTY, TERM unset/empty/dumb, too small, ok).
- `Input` widget: insert, backspace, delete, cursor movement, unicode.
- `App::handle_key` on a fixture plan (the one used by the ops tests): navigation and scrolling bounds; rename; `[`/`]`; remove; add at time (single and multi-file); add at silence; missing view placing a chapter with its preset title; tags form; jump (time, number, out-of-range); undo/redo including redo cleared by a new edit; refusals set the status and leave the plan unchanged; quit prompt outcomes (no changes quits at once; changes show the prompt; `s`/`d`/Esc).
- Rendering with `TestBackend` at 80x24 and 60x15: the table rows, selected-row marker, detail text wrapped, footer text, a popup, and no panic at the minimum size or with a long title.
- `ops` tests move unchanged and keep passing; the existing integration test (`edit` needs a terminal when stdin is piped) keeps passing; `edit --simple` and `run --simple` parse.
- Manual, in a real terminal (a subagent cannot drive one): edit a book over `ssh localhost` inside tmux, resize the window, press every key once, confirm the terminal is restored after `q` and after Ctrl-c.

## Out of scope

Mouse support, an in-TUI player, multi-select and bulk operations, themes and colour configuration, non-interactive edit commands (a separate sub-project).
