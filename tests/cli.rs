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
