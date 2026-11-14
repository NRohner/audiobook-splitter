# Resumable transcription (part B, sub-project 1)

Date: 2026-11-14. Follows `2026-11-14-cli-parity-design.md` (part A, done).

## Goal

A dropped SSH session or Ctrl-C during a long `analyze` must not throw away hours of Whisper work, and re-analyzing a book with a different `--threshold` should not re-transcribe it. Both the CLI and the GUI benefit.

## Decisions

- **Shared on-disk cache**, keyed by audio file and model, not a per-run checkpoint file. Re-analysis with new settings and `rescan` reuse it too.
- **Contents:** transcripts and per-file silence lists.
- **Control:** `cache info`, `cache clear [path]`, `--no-cache`, `AUDIOBOOK_CACHE_DIR`. No expiry and no size cap (out of scope); entries are keyed so stale ones are simply never hit.

## Architecture

New `src/cache.rs` in the lib. `Cache` is passed explicitly (`&Cache`) into the pipeline functions; there is no global.

```
Cache::open(dir: Option<PathBuf>) -> Cache      // None = disabled (all lookups miss, nothing stored)
Cache::default_dir() -> Option<PathBuf>         // AUDIOBOOK_CACHE_DIR, else the platform cache folder
cache.silences(path, min_silence, noise_db) -> Option<(Vec<Silence>, f64)>
cache.store_silences(path, min_silence, noise_db, &[Silence], duration)
cache.transcripts(path, model, &[Candidate]) -> Vec<Option<(String, String)>>   // one entry per candidate, None = miss
cache.store_transcripts(path, model, &[(Candidate, (String, String))])
cache.info() -> CacheInfo { dir, entries, bytes }
cache.clear(path: Option<&Path>) -> Result<usize, String>   // number of entries removed
```

### Location

`AUDIOBOOK_CACHE_DIR` if set; else `~/Library/Caches/audiobook-splitter` (macOS), `$XDG_CACHE_HOME/audiobook-splitter` or `~/.cache/audiobook-splitter` (Linux), `%LOCALAPPDATA%\audiobook-splitter\cache` (Windows). Built from environment variables, no new dependency. If no location can be determined, or it cannot be created or written, the cache silently disables itself for the run and the pipeline emits one `Log` line saying so; it never fails an analysis.

### Files

One JSON file per audio file: `<fingerprint>.json` where the fingerprint is the FNV-1a 64-bit hash (hex) of `absolute path | size | modified time (seconds)`. FNV-1a is implemented inline (the standard hasher is not stable across Rust versions). Content:

```
{ "version": 1,
  "silences":    { "<min_silence>|<noise_db>": { "duration": f64, "silences": [{start,end,duration}] } },
  "transcripts": { "<model id>|<clip_seconds>": { "<start_ms>|<end_ms>": ["before text", "after text"] } } }
```

- Model id = model file name + size in bytes. `clip_seconds` = `chapters::CLIP_SECONDS`, so changing it invalidates old transcripts.
- Float keys use fixed formatting (`{:.3}` for settings, integer milliseconds for positions), so the same value always produces the same key.
- Writes go to a temp file and are renamed (same as the plan file). A file that is missing, unparsable or has a newer `version` is treated as empty and overwritten on the next store.
- Concurrency: one process writes at a time per book in practice; the pipeline's worker threads reach the cache only through the single-threaded wrapper below. Two simultaneous `analyze` runs on the same book may overwrite each other's entries; the loser's work is simply re-done later.

## Pipeline changes

- `analyze(inputs, settings, epub, cache, emit)`: for each file, `cache.silences(..)` first; on a hit skip `detect_silences` and log "`<name>`: silences from cache". On a miss run FFmpeg and `store_silences`.
- New `pipeline::transcribe_cached(files, cands, transcriber_run, cache, model, emit)` in place of the direct `transcribe_candidates` call in `match_with_epub` and `rescan_one`:
  1. look up hits for all candidates;
  2. transcribe only the misses, in chunks of 64 candidates (`CHUNK`), through the `transcriber_run` closure (`FnMut(&[Candidate]) -> Result<Vec<(String, String)>, String>`; the real one calls `transcribe_candidates`);
  3. `store_transcripts` after each chunk, so a crash loses at most one chunk;
  4. return results in the original candidate order; emit `Tick` counting cached items as done, and one `Log` line "Reused N of M transcripts from cache." when N > 0.
- `rescan_one` caches only its transcripts; its `detect_silences_between` windows vary per call, so they are not cached.
- GUI: passes `Cache::open(Cache::default_dir())`; no UI change. It sets the cache once at start-up and shares it through `Arc`.

## CLI

- `--no-cache` on `analyze`, `rescan` and `run` (a field on the shared `Detect` group for `analyze`/`run`, a plain flag on `rescan`) opens `Cache::open(None)`.
- `cache info`: prints the folder, number of entries (files) and total size; prints "Cache folder: none available" when disabled.
- `cache clear [path]`: no argument removes every entry; with an audio file path (or a folder, expanded with `collect_inputs`) removes only those files' entries. Prints how many entries were removed; nothing to clear is not an error.

## Errors and edge cases

- The cache must never change results: a hit returns exactly what a miss would have computed (same settings, same model, same clip length). A changed audio file (size or modified time) changes the fingerprint and misses.
- Model swap: different model name or size misses; both sets of transcripts coexist in the file.
- Corrupt or unwritable cache: ignored, with at most one log line per run.
- Ctrl-C mid-chunk loses that chunk only; the next run resumes from the last stored chunk.

## Testing (tests first)

- `cache`: round trip for silences and transcripts; key sensitivity (different min_silence, noise_db, model, start/end ms, modified time) all miss; corrupt and newer-version files are treated as empty; disabled cache never stores; `clear(path)` removes only that file's entry; `info` counts entries and bytes.
- `transcribe_cached` with a fake `transcriber_run`: hits are skipped and results keep candidate order; only misses reach the closure; chunking boundaries (0, 1, 64, 65 misses); a failure in chunk 2 returns the error but chunk 1 stays stored; a second call hits everything and never calls the closure.
- `analyze` on generated audio twice with a temp cache: second run reports silences from cache and returns identical findings.
- CLI integration: `analyze` twice with `AUDIOBOOK_CACHE_DIR` set to a temp folder (second run's stderr mentions the cache); `cache info` shows entries; `cache clear` empties it; `--no-cache` leaves the folder untouched.
- Part A's tests keep passing; the GUI smoke test (curl analyze and split) is repeated once after the change.

## Out of scope

Size caps and eviction, caching decoded audio, caching `detect_silences_between` windows, a GUI control for the cache, sharing a cache between machines.
