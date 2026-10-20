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
fn cli_get_args_matches_the_boxlang_cli_scripting_docs() {
    // docs: `boxlang hello.bxs hola luis=majano --test`
    let file = script("docs.bxs");
    let (ok, out, _) = run(&[file.to_str().unwrap(), "hola", "luis=majano", "--test"]);
    assert!(ok);
    assert!(out.contains(r#""positionals":["hola","luis=majano"]"#), "{out}");
    assert!(out.contains(r#""options":{"test":true}"#), "{out}");
}

#[test]
fn option_values_need_equals_and_formats_follow_boxlang() {
    let file = script("formats.bxs");
    let (ok, out, _) = run(&[
        file.to_str().unwrap(), "--name", "luis", "--debug", "--!verbose", "--no-color", "--config=prod.json",
        "--message=\"Hello World\"", "-o='/path/to/file'", "-abc", "my/path/template", "--", "last",
    ]);
    assert!(ok);
    assert!(out.contains(r#""name":true"#), "--name does not take the next token: {out}");
    assert!(out.contains(r#""debug":true"#) && out.contains(r#""verbose":false"#) && out.contains(r#""color":false"#), "{out}");
    assert!(out.contains(r#""config":"prod.json""#) && out.contains(r#""message":"Hello World""#), "{out}");
    assert!(out.contains(r#""o":"/path/to/file""#), "{out}");
    assert!(out.contains(r#""a":true"#) && out.contains(r#""b":true"#) && out.contains(r#""c":true"#), "{out}");
    assert!(out.contains(r#""positionals":["luis","my/path/template","last"]"#), "{out}");
}

#[test]
fn unknown_options_before_the_file_do_not_run_the_script_like_boxlang() {
    // BoxLang treats the first unrecognized token as the target; a missing target is an error.
    let file = script("unknown.bxs");
    let (ok, out, _) = run(&["--bogus", file.to_str().unwrap()]);
    assert!(!ok);
    assert!(out.is_empty(), "the script must not run: {out}");
}

#[test]
fn help_and_version_are_global_only_as_the_sole_argument() {
    let (ok, out, _) = run(&["--help"]);
    assert!(ok && out.contains("Usage: matchbox"));
    let (ok, out, _) = run(&["-h"]);
    assert!(ok && out.contains("Usage: matchbox"));
    let (ok, out, _) = run(&["-v"]);
    assert!(ok && out.contains("matchbox version"));
    let (ok, out, _) = run(&["--version"]);
    assert!(ok && out.contains("matchbox version"));
    // a script and its own -h / -v
    let file = script("sole.bxs");
    let (ok, out, _) = run(&[file.to_str().unwrap(), "-h"]);
    assert!(ok && !out.contains("Usage: matchbox") && out.contains(r#""h":true"#), "{out}");
}
