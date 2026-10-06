#![cfg(all(not(target_family = "wasm"), not(target_os = "espidf")))]
use matchbox_vm::{
    bundle::{Artifact, TARGET, hash},
    types::{BxVM, BxValue},
    vm::VM,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

fn library() -> &'static PathBuf {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let target = root.join("target/plugin-fixture");
        let output = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(root.join("tests/native_plugins/Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.join("debug").join(format!(
            "{}matchbox_plugin_fixture{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    })
}
fn artifact() -> Artifact {
    let path = library();
    let bytes = fs::read(path).unwrap();
    Artifact {
        target: TARGET.into(),
        abi: 1,
        filename: path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256: hash(&bytes),
        bytes,
    }
}
fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_matchbox"))
        .current_dir(dir)
        .env("MATCHBOX_NATIVE_CACHE", dir.join("cache"))
        .args(args)
        .output()
        .unwrap()
}
fn success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn package(dir: &Path) -> PathBuf {
    let module = dir.join("module");
    fs::create_dir_all(module.join("bifs")).unwrap();
    fs::write(
        module.join("ModuleConfig.bx"),
        r#"
class {
    property name="moduleRecord";
    function configure() {
        assert moduleRecord.name == "module";
        assert moduleRecord.invocationPath == "bxModules.module";
        assert !moduleRecord.isActivated();
        assert boxRuntime.getModuleService().getModuleRecord("module") == moduleRecord;
        settings = { width: 1, nested: { first: 1, second: 2 } };
    }
    function onLoad() {
        assert getModuleSettings("module").width == 32;
        assert moduleRecord.settings.width == 32;
        assert moduleRecord.asStruct().name == "module";
        assert !moduleRecord.isActivated();
        println("loaded");
    }
    function onUnload() { println("unloaded"); }
}
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/SharedDouble.bx"),
        r#"class {
            function invoke(required numeric value) {
                assert moduleRecord.name == "module";
                assert moduleRecord.settings.width == 32;
                assert functionService.hasGlobalFunction("SharedDouble");
                assert !functionService.hasGlobalFunction("ordinaryUserFunction");
                assert boxRuntime.getFunctionService() == functionService;
                assert arrayFindNoCase(functionService.getGlobalFunctionNames(), "SharedDouble") > 0;
                return value * 2;
            }
        }"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/GenericInfo.bx"),
        r#"
@BoxMember({any: {name: "info"}})
class { function invoke(required any value) { return {value:-9}; } }
"#,
    )
    .unwrap();
    let artifact = artifact();
    fs::write(module.join(&artifact.filename), &artifact.bytes).unwrap();
    fs::write(module.join("box.json"), serde_json::json!({"slug":"fixture","type":"boxlang-modules","matchbox":{"artifacts":{TARGET:{"path":artifact.filename,"sha256":artifact.sha256,"abi":1}}}}).to_string()).unwrap();
    fs::write(
        dir.join("boxlang.json"),
        r#"{"modules":{"module":{"settings":{"width":32,"nested":{"second":9}}}}}"#,
    )
    .unwrap();
    module
}

#[test]
fn plugin_values_errors_and_object_lifetime() {
    let mut vm = VM::new();
    unsafe { matchbox_vm::plugins::load(&mut vm, library()) }.unwrap();
    // A nested ordinary BIF must not release the outer native invocation's GC guard.
    vm.gc_suspended = true;
    vm.call_function("isNull", vec![BxValue::new_null()], None)
        .unwrap();
    assert!(vm.gc_suspended);
    vm.gc_suspended = false;
    let object = vm.call_function("fixtureNew", vec![], None).unwrap();
    let result = vm.call_method_value(object, "info", vec![]).unwrap();
    let id = result.as_gc_id().unwrap();
    assert_eq!(vm.struct_get(id, "value").as_number(), 42.0);
    // Keep the callback globals and returned struct alive while collecting the temporary plugin object.
    let mut roots = vm.global_values.clone();
    roots.push(result);
    vm.heap.collect(&roots);
    assert_eq!(vm.struct_get(id, "value").as_number(), 42.0);
    assert!(
        vm.call_function("fixtureDrops", vec![], None)
            .unwrap()
            .as_number()
            >= 1.0
    );
    assert!(
        vm.call_function("fixturePanic", vec![], None)
            .unwrap_err()
            .to_string()
            .contains("panicked")
    );
    assert!(vm.call_function("fixtureBadHandle", vec![], None).is_err());
    assert!(
        vm.call_function("fixtureInfo", vec![BxValue::new_number(1.0)], None)
            .is_err()
    );
    assert!(vm.call_function("fixtureInfo", vec![], None).is_err());
    let object = vm.call_function("fixtureNew", vec![], None).unwrap();
    vm.insert_global("retainedObject".into(), object);
    // A separate provider instance cannot interpret another library's private payload.
    unsafe { matchbox_vm::plugins::load(&mut vm, library()) }.unwrap();
    assert!(vm.call_function("fixtureInfo", vec![object], None).is_err());
    assert!(vm.call_method_value(object, "info", vec![]).is_ok());
}

#[test]
fn retained_values_survive_gc_and_release_without_a_live_vm() {
    let mut vm = VM::new();
    unsafe { matchbox_vm::plugins::load(&mut vm, library()) }.unwrap();
    let payload = vm.struct_new();
    let text = vm.string_new("retained text".into());
    vm.struct_set(payload, "text", BxValue::new_ptr(text));
    vm.call_function("fixtureStore", vec![BxValue::new_ptr(payload)], None)
        .unwrap();
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    assert_eq!(
        vm.call_function("fixtureRecall", vec![], None).unwrap(),
        BxValue::new_ptr(payload)
    );
    assert_eq!(
        vm.to_string(vm.struct_get(payload, "text")),
        "retained text"
    );
    let changed = vm.string_new("changed".into());
    vm.struct_set(payload, "text", BxValue::new_ptr(changed));
    for _ in 0..8 {
        vm.collect_garbage_now();
    }
    assert_eq!(vm.to_string(vm.struct_get(payload, "text")), "changed");
    vm.call_function("fixtureStore", vec![BxValue::new_number(42.0)], None)
        .unwrap();
    for _ in 0..8 {
        vm.collect_garbage_now();
    }
    assert!(vm.heap.get_opt(payload).is_none());
    assert!(vm.heap.get_opt(changed).is_none());
    assert_eq!(
        vm.call_function("fixtureRecall", vec![], None)
            .unwrap()
            .as_number(),
        42.0
    );
    unsafe { matchbox_vm::plugins::load(&mut vm, library()) }.unwrap();
    assert!(
        vm.call_function("fixtureRecall", vec![], None)
            .unwrap_err()
            .to_string()
            .contains("another plugin provider")
    );
    vm.call_function("fixtureClear", vec![], None).unwrap();
    assert!(vm.call_function("fixtureRecall", vec![], None).is_err());

    // The containing native object owns the root and releases it without a call context.
    let payload = vm.struct_new();
    let holder = vm
        .call_function("fixtureHold", vec![BxValue::new_ptr(payload)], None)
        .unwrap();
    vm.insert_global("heldValue".into(), holder);
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    assert_eq!(
        vm.call_method_value(holder, "get", vec![]).unwrap(),
        BxValue::new_ptr(payload)
    );
    vm.insert_global("heldValue".into(), BxValue::new_null());
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    assert!(vm.heap.get_opt(payload).is_none());

    let payload = vm.struct_new();
    vm.call_function("fixtureStore", vec![BxValue::new_ptr(payload)], None)
        .unwrap();
    drop(vm);
    let mut other_vm = VM::new();
    unsafe { matchbox_vm::plugins::load(&mut other_vm, library()) }.unwrap();
    assert!(
        other_vm
            .call_function("fixtureRecall", vec![], None)
            .unwrap_err()
            .to_string()
            .contains("another VM")
    );
    other_vm
        .call_function("fixtureClear", vec![], None)
        .unwrap();
    other_vm
        .call_function("fixtureStore", vec![BxValue::new_null()], None)
        .unwrap();
    assert!(
        other_vm
            .call_function("fixtureRecall", vec![], None)
            .unwrap()
            .is_null()
    );
    other_vm
        .call_function("fixtureClear", vec![], None)
        .unwrap();
}

#[test]
fn retained_callback_keeps_its_captured_receiver_alive() {
    let mut vm = VM::new();
    unsafe { matchbox_vm::plugins::load(&mut vm, library()) }.unwrap();
    let source = r#"
class Receiver {
    this.label = "captured receiver";
    function callback() { return () => this.label; }
}
receiver = new Receiver();
fixtureStore(receiver.callback());
receiver = null;
"#;
    let ast = matchbox_compiler::parser::parse(source, Some("retained-callback.bxs")).unwrap();
    let chunk = matchbox_compiler::compiler::Compiler::new("retained-callback.bxs")
        .compile(&ast, source)
        .unwrap();
    vm.interpret_sync(chunk).unwrap();
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    let callback = vm.call_function("fixtureRecall", vec![], None).unwrap();
    let result = vm
        .call_function_by_value(
            &callback,
            vec![],
            std::rc::Rc::new(std::cell::RefCell::new(matchbox_vm::Chunk::default())),
        )
        .unwrap();
    assert_eq!(vm.to_string(result), "captured receiver");
    vm.call_function("fixtureClear", vec![], None).unwrap();
}

#[test]
fn module_contract_direct_bytecode_and_standalone() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path());
    fs::write(
        dir.path().join("app.bxs"),
        r#"
function ordinaryUserFunction() { return "user function"; }
assert sharedDouble(value=4) == 8;
assert fixtureCreate().info().value == 42;
assert (3).info().value == -9;
object = fixtureNew(VALUE=32);
assert fixtureInfo(object).value == 32;
assert object.info().value == 32;
fixtureInfo = function(object) { return {value:-1}; };
assert object.info().value == 32;
assert getModuleSettings("module").width == 32;
assert getModuleSettings("module").nested.first == 1;
assert getModuleSettings("module").nested.second == 9;
println("application");
"#,
    )
    .unwrap();
    let direct = cli(dir.path(), &["app.bxs", "--module", "module"]);
    success(&direct);
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout).trim(),
        "loaded\napplication\nunloaded"
    );
    // Build/packaging must not execute either lifecycle hook.
    let built = cli(
        dir.path(),
        &[
            "--build", "app.bxs", "--module", "module", "--output", "app.bxb",
        ],
    );
    success(&built);
    assert!(!String::from_utf8_lossy(&built.stdout).contains("loaded"));
    let bytecode = cli(dir.path(), &["app.bxb"]);
    success(&bytecode);
    let built = cli(
        dir.path(),
        &[
            "--target",
            "native",
            "app.bxs",
            "--module",
            "module",
            "--output",
            "application",
        ],
    );
    success(&built);
    assert!(!String::from_utf8_lossy(&built.stdout).contains("loaded"));
    fs::remove_dir_all(dir.path().join("module")).unwrap();
    fs::remove_file(dir.path().join("app.bxs")).unwrap();
    let execute = || {
        Command::new(dir.path().join("application"))
            .current_dir(dir.path())
            .env("MATCHBOX_NATIVE_CACHE", dir.path().join("cache"))
            .output()
            .unwrap()
    };
    let first = execute();
    success(&first);
    let second = execute();
    success(&second);
    assert_eq!(first.stdout, direct.stdout);
    assert_eq!(second.stdout, first.stdout);
}

#[test]
fn cache_is_atomic_verified_and_rejects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = artifact();
    let paths = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| artifact.extract(dir.path()).unwrap()))
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(paths.iter().all(|p| p == &paths[0]));
    let timestamp = fs::metadata(&paths[0]).unwrap().modified().unwrap();
    assert_eq!(artifact.extract(dir.path()).unwrap(), paths[0]);
    assert_eq!(
        fs::metadata(&paths[0]).unwrap().modified().unwrap(),
        timestamp
    );
    fs::write(&paths[0], b"tampered").unwrap();
    assert!(artifact.extract(dir.path()).is_err());
    let mut wrong = artifact.clone();
    wrong.target = "wrong-target".into();
    assert!(wrong.extract(dir.path()).is_err());
    let mut wrong = artifact.clone();
    wrong.abi = 999;
    assert!(wrong.extract(dir.path()).is_err());
    let mut wrong = artifact;
    wrong.filename = "../escape.so".into();
    assert!(wrong.extract(dir.path()).is_err());
}

#[test]
fn missing_capability_and_error_shutdown_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path());
    fs::write(dir.path().join("app.bxs"), "throw 'application failure';").unwrap();
    let output = cli(dir.path(), &["app.bxs", "--module", "module"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("unloaded"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("application failure"));
    let output = Command::new(env!("CARGO_BIN_EXE_matchbox"))
        .current_dir(dir.path())
        .env("MATCHBOX_FIXTURE_MISSING_CAPABILITY", "1")
        .env("MATCHBOX_NATIVE_CACHE", dir.path().join("cache"))
        .args(["app.bxs", "--module", "module"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing required capability"));
}

#[test]
fn module_names_mappings_dependencies_and_disabled_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["consumer-dir", "provider-dir", "disabled"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    fs::write(
        dir.path().join("consumer-dir/box.json"),
        r#"{"boxlang":{"moduleName":"consumer"}}"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("provider-dir/box.json"),
        r#"{"boxlang":{"moduleName":"provider"}}"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("consumer-dir/ModuleConfig.bx"),
        r#"
class {
    this.mapping = "consumerMapping";
    this.dependencies = ["provider"];
    function configure() { settings = {}; }
    function onLoad() { assert getModuleSettings("provider").ready; println("consumer loaded"); }
    function onUnload() { println("consumer unloaded"); }
}
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("provider-dir/ModuleConfig.bx"),
        r#"
class {
    function configure() { settings = {ready:true}; }
    function onLoad() { println("provider loaded"); }
    function onUnload() { println("provider unloaded"); }
}
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("disabled/ModuleConfig.bx"),
        "class { this.enabled = false; throw 'disabled module executed'; }",
    )
    .unwrap();
    fs::create_dir(dir.path().join("consumer-dir/models")).unwrap();
    fs::write(
        dir.path().join("consumer-dir/models/Value.bx"),
        "class { function get() { return 7; } }",
    )
    .unwrap();
    fs::write(dir.path().join("app.bxs"), "import bxModules.consumerMapping.models.Value as Value; instance = new Value(); assert instance.get() == 7;").unwrap();
    let args = [
        "app.bxs",
        "--module",
        "consumer-dir",
        "--module",
        "provider-dir",
        "--module",
        "disabled",
    ];
    let output = cli(dir.path(), &args);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "provider loaded\nconsumer loaded\nconsumer unloaded\nprovider unloaded"
    );
    fs::write(
        dir.path().join("provider-dir/ModuleConfig.bx"),
        "class { this.dependencies = ['consumer']; }",
    )
    .unwrap();
    let output = cli(dir.path(), &args);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Circular module dependency"));
    fs::remove_dir_all(dir.path().join("provider-dir")).unwrap();
    let output = cli(dir.path(), &["app.bxs", "--module", "consumer-dir"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires missing module provider"));
}

#[test]
fn shared_bif_annotations_bind_members_independently_of_global_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("boxlang_modules/annotations");
    fs::create_dir_all(module.join("bifs")).unwrap();
    fs::write(module.join("ModuleConfig.bx"), "class {}").unwrap();
    fs::write(
        module.join("bifs/StringDecorate.bx"),
        r#"
@BoxBIF( ["decorate", "decorateAlias"] )
@BoxMember( {string: {name: "decorate", objectArgument: "text"}} )
class {
    function invoke(required string prefix, required string text, string suffix = "!") {
        return prefix & text & suffix;
    }
}
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/ArrayLabel.bx"),
        r#"
@BoxBIF( "onlyArrayLabel" )
@BoxMember( "array" )
class { function invoke(required array value) { return "array-" & arrayLen(value); } }
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/StructLabel.bx"),
        r#"
class boxBIF="onlyStructLabel" boxMember={struct: {}} {
    function invoke(required struct value) { return "struct-" & structCount(value); }
}
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/IntegerLabel.bx"),
        r#"
@BoxBIF("signedLabel", "signedAlias")
@BoxMember("integer")
class { function invoke(required integer value) { return "integer-" & value; } }
"#,
    )
    .unwrap();
    fs::write(dir.path().join("app.bxs"), r#"
function check(required boolean condition) { if (!condition) { throw "annotation assertion failed"; } }
check(stringDecorate("[", "value") == "[value!");
check(decorateAlias(TEXT="value", PREFIX="[", SUFFIX="]") == "[value]");
check("value".decorate("[") == "[value!");
check("value".decorate(SUFFIX="]", PREFIX="[") == "[value]");
check([1,2].label() == "array-2");
check({}.label() == "struct-0");
check(onlyArrayLabel([1]) == "array-1");
check(onlyStructLabel({a:1}) == "struct-1");
check(integerLabel(3) == "integer-3");
check(signedAlias(3) == "integer-3");
check(signedLabel(3) == "integer-3");
check((3).label() == "integer-3");
check("3".label() == "integer-3");
check((42).decorate("[") == "[42!");
decorate = function() { return "replaced"; };
decorateAlias = decorate;
stringDecorate = decorate;
check("value".decorate("[") == "[value!");
check(!isDefined("arrayLabel"));
check(!isDefined("structLabel"));
println("annotations OK");
"#).unwrap();
    let output = cli(dir.path(), &["app.bxs"]);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "annotations OK"
    );
    success(&cli(
        dir.path(),
        &["--build", "app.bxs", "--output", "app.bxb"],
    ));
    fs::remove_dir_all(dir.path().join("boxlang_modules")).unwrap();
    let output = cli(dir.path(), &["app.bxb"]);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "annotations OK"
    );
}

#[test]
fn startup_failures_unwind_partial_modules_and_preserve_the_original_error() {
    for phase in ["configure", "onLoad", "application"] {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            let module = dir.path().join("boxlang_modules").join(name);
            fs::create_dir_all(&module).unwrap();
            let dependency = if name == "b" {
                "this.dependencies = ['a'];"
            } else {
                ""
            };
            let fail_configure = if name == "b" && phase == "configure" {
                "throw 'startup failure';"
            } else {
                ""
            };
            let fail_load = if name == "b" && phase == "onLoad" {
                "throw 'startup failure';"
            } else {
                ""
            };
            let fail_unload = if name == "b" {
                "throw 'shutdown failure';"
            } else {
                ""
            };
            fs::write(
                module.join("ModuleConfig.bx"),
                format!(
                    r#"
class {{
    {dependency}
    function configure() {{ settings = {{}}; {fail_configure} }}
    function onLoad() {{ assert !moduleRecord.isActivated(); {fail_load} println("load-{name}"); }}
    function onUnload() {{
        println("unload-{name}-" & moduleRecord.isActivated());
        {fail_unload}
    }}
}}
"#
                ),
            )
            .unwrap();
        }
        fs::write(dir.path().join("app.bxs"), "throw 'application failure';").unwrap();
        let output = cli(dir.path(), &["app.bxs"]);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(if phase == "application" {
                "application failure"
            } else {
                "startup failure"
            }),
            "{stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected = match phase {
            "configure" => "unload-b-false\nunload-a-false",
            "onLoad" => "load-a\nunload-b-false\nunload-a-true",
            _ => "load-a\nload-b\nunload-b-true\nunload-a-true",
        };
        assert_eq!(stdout.trim(), expected);
    }
}

#[test]
fn runtime_home_modules_are_discovered_and_project_packages_take_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let global = home.join("modules/location");
    let local = dir.path().join("boxlang_modules/location");
    fs::create_dir_all(&global).unwrap();
    fs::write(
        global.join("ModuleConfig.bx"),
        "class { function configure() { settings = {source:'home'}; } }",
    )
    .unwrap();
    fs::write(
        dir.path().join("app.bxs"),
        "println(getModuleSettings('location').source);",
    )
    .unwrap();
    let execute = || {
        Command::new(env!("CARGO_BIN_EXE_matchbox"))
            .current_dir(dir.path())
            .env("BOXLANG_HOME", &home)
            .args(["app.bxs"])
            .output()
            .unwrap()
    };
    let output = execute();
    success(&output);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "home");
    fs::create_dir_all(&local).unwrap();
    fs::write(
        local.join("ModuleConfig.bx"),
        "class { function configure() { settings = {source:'project'}; } }",
    )
    .unwrap();
    let output = execute();
    success(&output);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "project");
}

#[test]
fn source_native_modules_still_build_through_fusion() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = cli(
        dir.path(),
        &[
            "--target",
            "native",
            root.join("tests/scripts/vm_module_native_fusion.bxs")
                .to_str()
                .unwrap(),
            "--module",
            root.join("tests/modules/native-math").to_str().unwrap(),
            "--output",
            "fusion-app",
        ],
    );
    success(&output);
    let output = Command::new(dir.path().join("fusion-app"))
        .current_dir(dir.path())
        .output()
        .unwrap();
    success(&output);
}
