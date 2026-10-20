//! `matchbox file.bxs ...`: options only count before the file, everything after is the script's.

use std::{fs, path::PathBuf, process::Command};

fn script(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("matchbox-cli-args-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, "println( serializeJSON( cliGetArgs() ) );\n").unwrap();
    path
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_matchbox")).args(args).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn script_arguments_are_not_matchbox_options() {
    let file = script("args.bxs");
    let file = file.to_str().unwrap();
    let (ok, out, _) = run(&[file, "-v", "--build", "-h"]);
    assert!(ok);
    assert!(!out.contains("matchbox version"), "{out}");
    assert!(out.contains(r#""v":true"#) && out.contains(r#""build":true"#) && out.contains(r#""h":true"#), "{out}");
    assert!(!PathBuf::from(file).with_extension("bxb").exists());
}

#[test]
fn the_first_script_argument_is_kept() {
    let file = script("first.bxs");
    let (ok, out, _) = run(&[file.to_str().unwrap(), "one", "two"]);
    assert!(ok);
    assert!(out.contains(r#""positionals":["one","two"]"#), "{out}");
    let (ok, out, _) = run(&[file.to_str().unwrap(), "--", "one", "--verbose"]);
    assert!(ok);
    assert!(out.contains(r#""positionals":["one"]"#) && out.contains(r#""verbose":true"#), "{out}");
}

#[test]
fn unknown_options_before_the_file_fail_with_the_help_text() {
    let file = script("unknown.bxs");
    let (ok, out, err) = run(&["--bogus", file.to_str().unwrap()]);
    assert!(!ok);
    assert!(err.contains("Unknown option '--bogus'"), "{err}");
    assert!(err.contains("Usage: matchbox"), "{err}");
    assert!(out.is_empty(), "the script must not run: {out}");
}

#[test]
fn help_and_version_still_work_before_the_file() {
    let (ok, out, _) = run(&["--help"]);
    assert!(ok && out.contains("Usage: matchbox"));
    let (ok, out, _) = run(&["-v"]);
    assert!(ok && out.contains("matchbox version"));
}
