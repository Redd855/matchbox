use std::process::Command;

fn render(template: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("page.bxm");
    std::fs::write(&path, template).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_matchbox")).arg(&path).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn counting_loop_stops_at_to() {
    assert_eq!(render(r##"<bx:loop from="1" to="3" index="i">x</bx:loop>"##), "xxx");
    assert_eq!(render(r##"<bx:output><bx:loop from="1" to="3" index="i">#i#</bx:loop></bx:output>"##), "123");
    assert_eq!(render(r##"<bx:output><bx:loop from="10" to="1" step="-3" index="i">#i#,</bx:loop></bx:output>"##), "10,7,4,1,");
}

#[test]
fn array_list_and_struct_loops_visit_each_entry() {
    assert_eq!(render(r##"<bx:set items = ["a","b"]><bx:loop array="#items#" item="it">x</bx:loop>"##), "xx");
    assert_eq!(render(r##"<bx:output><bx:loop array="#[7,8]#" item="v" index="n">#n#=#v#;</bx:loop></bx:output>"##), "1=7;2=8;");
    assert_eq!(render(r##"<bx:output><bx:loop list="a,b,c" item="x">#x#</bx:loop></bx:output>"##), "abc");
    assert_eq!(render(r##"<bx:set s = {a:1,b:2}><bx:output><bx:loop collection="#s#" item="k">#k#</bx:loop></bx:output>"##), "ab");
}

#[test]
fn condition_loop_runs_while_true() {
    assert_eq!(
        render(r##"<bx:set n = 0><bx:output><bx:loop condition="n LT 3"><bx:set n = n + 1>#n#,</bx:loop></bx:output>"##),
        "1,2,3,"
    );
}
