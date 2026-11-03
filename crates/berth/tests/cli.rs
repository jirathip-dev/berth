//! The CLI's output, end to end.

use std::process::Command;

fn berth(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_berth"))
        .args(args)
        .output()
        .expect("berth runs");
    assert!(out.status.success(), "berth {args:?} failed: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn hello_prints_the_version() {
    assert_eq!(
        berth(&["hello"]),
        format!("berth {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn version_flag_prints_the_version() {
    assert_eq!(
        berth(&["--version"]),
        format!("berth {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_hello() {
    assert!(berth(&["--help"]).contains("hello"));
}
