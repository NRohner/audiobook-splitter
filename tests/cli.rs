mod common;
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::str::contains;

#[test]
fn no_arguments_prints_help() {
    cargo_bin_cmd!("audiobook-splitter-rust")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage"))
        .stderr(contains("gui"));
}

use std::fs;

fn bin() -> assert_cmd::Command {
    cargo_bin_cmd!("audiobook-splitter-rust")
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

// A 15 s book with two silences, analyzed into `dir/plan.json`.
fn analyzed(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = common::temp_dir(name);
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let plan = dir.join("plan.json");
    bin().arg("analyze").arg(&wav).args(["--min-silence", "1", "-o"]).arg(&plan).assert().success();
    (dir, wav, plan)
}

#[test]
fn analyze_then_split_without_an_epub() {
    let dir = common::temp_dir("analyze-split");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let plan = dir.join("plan.json");
    bin()
        .arg("analyze")
        .arg(&wav)
        .args(["--min-silence", "1", "-o"])
        .arg(&plan)
        .assert()
        .success()
        .stdout(contains("Part 3"));
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).assert().success().stdout(contains("Wrote 3 file(s)"));
    assert_eq!(names(&out), ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav"]);
}

#[test]
fn split_flags_override_the_template() {
    let (dir, _, plan) = analyzed("template");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).args(["--template", "{n}_{title}", "--album", "Book"]).assert().success();
    assert_eq!(names(&out), ["00_Part 1.wav", "01_Part 2.wav", "02_Part 3.wav"]);
}

#[test]
fn analyze_rejects_bad_input_and_flags() {
    let dir = common::temp_dir("bad-input");
    bin().arg("analyze").arg(dir.join("nothere.wav")).assert().failure().stderr(contains("not a file or folder"));
    let wav = dir.join("a.wav");
    common::tone_with_gaps(&wav, 5, &[]);
    bin().arg("analyze").arg(&wav).args(["--min-silence", "0"]).assert().failure().stderr(contains("Minimum silence"));
    bin().arg("analyze").arg(&wav).args(["--threshold", "2"]).assert().failure().stderr(contains("between 0 and 1"));
    bin().arg("analyze").arg(&wav).args(["--noise-db", "-30", "--min-silence", "1", "-o"]).arg(dir.join("p.json")).assert().success();
}

#[test]
fn a_missing_ffmpeg_is_reported_before_any_work() {
    let dir = common::temp_dir("no-ffmpeg");
    let wav = dir.join("a.wav");
    common::tone_with_gaps(&wav, 5, &[]);
    bin().env("PATH", &dir).arg("analyze").arg(&wav).assert().failure().stderr(contains("was not found on your PATH"));
}

#[test]
fn a_plan_whose_audio_changed_is_rejected() {
    let (dir, wav, plan) = analyzed("changed");
    common::tone_with_gaps(&wav, 40, &[]);
    bin().arg("split").arg(&plan).arg("-o").arg(dir.join("out")).assert().failure().stderr(contains("has changed"));
}

#[test]
fn edit_needs_a_terminal() {
    let (_, _, plan) = analyzed("edit-tty");
    bin().arg("edit").arg(&plan).assert().failure().stderr(contains("interactive terminal"));
}

#[test]
fn rescan_needs_a_plan_made_with_an_epub() {
    let (_, _, plan) = analyzed("rescan-no-epub");
    bin().arg("rescan").arg(&plan).assert().failure().stderr(contains("without an EPUB"));
}

#[test]
fn preview_writes_a_clip_from_the_chosen_break() {
    let (dir, _, plan) = analyzed("preview");
    let clip = dir.join("clip.mp3");
    bin().arg("preview").arg(&plan).arg("2").arg("-o").arg(&clip).assert().success().stdout(contains("clip.mp3"));
    assert!(fs::read(&clip).unwrap().starts_with(b"ID3"));
    bin().arg("preview").arg(&plan).arg("9").arg("-o").arg(&clip).assert().failure().stderr(contains("no chapter 9"));
    bin().arg("preview").arg(&plan).arg("0").arg("-o").arg(&clip).assert().failure().stderr(contains("no chapter 0"));
}

#[test]
fn run_chains_analyze_and_split() {
    let dir = common::temp_dir("run");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let out = dir.join("out");
    bin().arg("run").arg(&wav).arg("-o").arg(&out).args(["--min-silence", "1", "--yes"]).assert().success().stdout(contains("Wrote 3 file(s)"));
    assert_eq!(names(&out), ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav", "plan.json"]);
}

#[test]
fn run_without_a_terminal_cannot_prompt() {
    bin().arg("run").assert().failure().stderr(contains("interactive terminal"));
}

#[test]
fn the_old_cli_flag_still_works_as_run() {
    bin().arg("--cli").assert().failure().stderr(contains("interactive terminal"));
}

#[test]
fn run_rejects_an_output_path_that_is_a_file_before_analyzing() {
    let dir = common::temp_dir("run-bad-out");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let out = dir.join("out.txt");
    fs::write(&out, "x").unwrap();
    bin().arg("run").arg(&wav).arg("-o").arg(&out).arg("--yes").assert().failure().stderr(contains("is not a folder"));
    assert_eq!(names(&dir), ["book.wav", "out.txt"]);
}

// Runs `analyze` with the cache in `cache` and returns stderr, where progress logs go.
fn analyze_stderr(wav: &std::path::Path, plan: &std::path::Path, cache: &std::path::Path, extra: &[&str]) -> String {
    let out = bin()
        .env("AUDIOBOOK_CACHE_DIR", cache)
        .arg("analyze")
        .arg(wav)
        .args(["--min-silence", "1", "-o"])
        .arg(plan)
        .args(extra)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn a_second_analyze_reuses_the_cached_silences() {
    let dir = common::temp_dir("cache-reuse");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    let first = analyze_stderr(&wav, &dir.join("a.json"), &cache, &[]);
    assert!(!first.contains("from cache"), "{}", first);
    let second = analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]);
    assert!(second.contains("silences from cache"), "{}", second);
    assert!(cache.is_dir());
}

#[test]
fn no_cache_leaves_the_cache_alone() {
    let dir = common::temp_dir("cache-off");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    analyze_stderr(&wav, &dir.join("a.json"), &cache, &["--no-cache"]);
    assert!(!cache.exists(), "--no-cache must not create the cache folder");
    analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]); // Fills it.
    let again = analyze_stderr(&wav, &dir.join("c.json"), &cache, &["--no-cache"]);
    assert!(!again.contains("from cache"), "{}", again);
}

#[test]
fn cache_info_and_clear() {
    let dir = common::temp_dir("cache-cmd");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    let cmd = |args: &[&str]| {
        let mut c = bin();
        c.env("AUDIOBOOK_CACHE_DIR", &cache).arg("cache").args(args);
        c
    };

    cmd(&["info"]).assert().success().stdout(contains("Cache folder:")).stdout(contains("0 book file(s)"));
    analyze_stderr(&wav, &dir.join("a.json"), &cache, &[]);
    cmd(&["info"]).assert().success().stdout(contains("1 book file(s)"));

    cmd(&["clear", wav.to_str().unwrap()]).assert().success().stdout(contains("Removed 1 cache entry"));
    cmd(&["clear", wav.to_str().unwrap()]).assert().success().stdout(contains("Removed 0 cache entries"));

    analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]);
    cmd(&["clear"]).assert().success().stdout(contains("Removed 1 cache entry"));
    cmd(&["info"]).assert().success().stdout(contains("0 book file(s)"));
}

#[test]
fn cache_clear_rejects_a_missing_path() {
    let dir = common::temp_dir("cache-missing");
    bin()
        .env("AUDIOBOOK_CACHE_DIR", dir.join("cache"))
        .args(["cache", "clear"])
        .arg(dir.join("nothere.wav"))
        .assert()
        .failure()
        .stderr(contains("not a file or folder"));
}
