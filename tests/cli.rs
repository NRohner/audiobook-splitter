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
