#![cfg(all(not(target_family = "wasm"), not(target_os = "espidf")))]
use matchbox_vm::{
    bundle::{Artifact, SharedLibrary, TARGET, hash},
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
        libraries: vec![],
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

#[cfg(target_os = "linux")]
fn linked_library() -> &'static (PathBuf, PathBuf) {
    static LIBRARY: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let target = root.join("target/plugin-fixture-libraries");
        let output = Command::new(env!("CARGO"))
            .args([
                "build",
                "--features",
                "bundled-libraries",
                "--message-format=json",
                "--manifest-path",
            ])
            .arg(root.join("tests/native_plugins/Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        success(&output);
        let out = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let message: serde_json::Value = serde_json::from_str(line).ok()?;
                (message["reason"] == "build-script-executed"
                    && message["package_id"]
                        .as_str()?
                        .contains("matchbox-plugin-fixture"))
                .then(|| PathBuf::from(message["out_dir"].as_str().unwrap()))
            })
            .expect("Fixture build did not report its native library directory");
        (target.join("debug/libmatchbox_plugin_fixture.so"), out)
    })
}

#[cfg(target_os = "linux")]
#[test]
fn bundled_libraries_resolve_transitive_dependencies_without_the_source_package() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("module");
    fs::create_dir_all(module.join("backends")).unwrap();
    fs::create_dir_all(module.join("vendor/deep")).unwrap();
    fs::write(module.join("ModuleConfig.bx"), "class {}").unwrap();
    let (plugin, dependencies) = linked_library();
    let plugin = fs::read(plugin).unwrap();
    fs::write(module.join("backends/provider.bin"), &plugin).unwrap();
    let mut libraries = Vec::new();
    for name in ["codec", "leaf"] {
        let filename = format!("libmatchbox_fixture_{name}.so");
        let bytes = fs::read(dependencies.join(&filename)).unwrap();
        let path = format!("vendor/deep/{name}.bin");
        fs::write(module.join(&path), &bytes).unwrap();
        if name == "leaf" {
            let alias = format!("vendor/deep/{filename}");
            std::os::unix::fs::symlink("leaf.bin", module.join(&alias)).unwrap();
            libraries.push(serde_json::json!({"path":alias,"sha256":hash(&bytes)}));
        } else {
            libraries
                .push(serde_json::json!({"path":path,"filename":filename,"sha256":hash(&bytes)}));
        }
    }
    let metadata = serde_json::json!({"matchbox":{"artifacts":{TARGET:{
        "path":"backends/provider.bin","filename":"provider.so","sha256":hash(&plugin),"abi":1,"libraries":libraries
    }}}});
    fs::write(module.join("box.json"), metadata.to_string()).unwrap();
    fs::write(
        dir.path().join("app.bxs"),
        "assert fixtureLibraryValue() == 42; println('bundled libraries OK');",
    )
    .unwrap();
    let mut missing = metadata.clone();
    missing["matchbox"]["artifacts"][TARGET]["libraries"] = serde_json::json!([]);
    fs::write(module.join("box.json"), missing.to_string()).unwrap();
    let output = cli(dir.path(), &["app.bxs", "--module", "module"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("libmatchbox_fixture_codec.so"));
    fs::write(module.join("box.json"), metadata.to_string()).unwrap();
    let direct = cli(dir.path(), &["app.bxs", "--module", "module"]);
    success(&direct);
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout).trim(),
        "bundled libraries OK"
    );
    success(&cli(
        dir.path(),
        &[
            "--build", "app.bxs", "--module", "module", "--output", "app.bxb",
        ],
    ));
    success(&cli(
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
    ));
    fs::remove_dir_all(&module).unwrap();
    fs::remove_file(dir.path().join("app.bxs")).unwrap();
    let bytecode = cli(dir.path(), &["app.bxb"]);
    success(&bytecode);
    assert_eq!(bytecode.stdout, direct.stdout);
    let standalone = Command::new(dir.path().join("application"))
        .current_dir(dir.path())
        .env("MATCHBOX_NATIVE_CACHE", dir.path().join("fresh-cache"))
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .output()
        .unwrap();
    success(&standalone);
    assert_eq!(standalone.stdout, direct.stdout);
}

#[test]
fn cache_is_atomic_verified_and_rejects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let mut artifact = artifact();
    artifact.libraries.push(SharedLibrary {
        filename: "codec.so".into(),
        sha256: hash(b"library"),
        bytes: b"library".to_vec(),
    });
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
    assert_eq!(
        fs::read(paths[0].parent().unwrap().join("codec.so")).unwrap(),
        b"library"
    );
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
fn library_groups_verify_cached_dependencies_and_change_identity_with_their_contents() {
    let dir = tempfile::tempdir().unwrap();
    let mut artifact = artifact();
    for filename in ["codec.so", "helper.so"] {
        artifact.libraries.push(SharedLibrary {
            filename: filename.into(),
            sha256: hash(filename.as_bytes()),
            bytes: filename.as_bytes().to_vec(),
        });
    }
    let original = artifact.extract(dir.path()).unwrap();
    let mut reordered = artifact.clone();
    reordered.libraries.reverse();
    assert_eq!(reordered.extract(dir.path()).unwrap(), original);
    let mut updated = artifact.clone();
    updated.libraries[0].bytes = b"new dependency".to_vec();
    updated.libraries[0].sha256 = hash(&updated.libraries[0].bytes);
    assert_ne!(
        updated.extract(dir.path()).unwrap().parent(),
        original.parent()
    );
    let mut renamed = artifact.clone();
    renamed.libraries[0].filename = "renamed.so".into();
    assert_ne!(
        renamed.extract(dir.path()).unwrap().parent(),
        original.parent()
    );
    fs::write(original.parent().unwrap().join("codec.so"), b"tampered").unwrap();
    assert!(
        artifact
            .extract(dir.path())
            .unwrap_err()
            .to_string()
            .contains("failed verification")
    );
    let mut invalid = artifact.clone();
    invalid.libraries[0].bytes.push(1);
    assert!(invalid.validate().is_err());
    invalid = artifact.clone();
    invalid.libraries[0].filename = artifact.filename.clone();
    assert!(invalid.validate().is_err());
    for filename in [
        "../escape.so",
        r"..\escape.dll",
        "C:escape.dll",
        "bad\0name",
        ".",
    ] {
        invalid.libraries[0].filename = filename.into();
        assert!(invalid.validate().is_err(), "accepted {filename:?}");
    }
    invalid = artifact;
    invalid.target = "x86_64-pc-windows-msvc".into();
    invalid.libraries[0].filename = "HELPER.SO".into();
    assert!(invalid.validate_files().is_err());
    #[cfg(unix)]
    {
        let cache = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), cache.path().join(TARGET)).unwrap();
        assert!(updated.extract(cache.path()).is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}

#[test]
fn dependent_library_metadata_rejects_unverified_or_escaping_files_before_startup() {
    let dir = tempfile::tempdir().unwrap();
    let module = package(dir.path());
    fs::write(module.join("codec.so"), b"library").unwrap();
    fs::write(dir.path().join("outside.so"), b"library").unwrap();
    fs::write(dir.path().join("app.bxs"), "println('application');").unwrap();
    let manifest = module.join("box.json");
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    for (library, error) in [
        (
            serde_json::json!({"path":"codec.so","sha256":hash(b"wrong")}),
            "hash does not match",
        ),
        (
            serde_json::json!({"path":"../outside.so","sha256":hash(b"library")}),
            "escapes module package",
        ),
        (
            serde_json::json!({"path":"codec.so","filename":"../escape.so","sha256":hash(b"library")}),
            "Invalid artifact filename",
        ),
        (
            serde_json::json!({"path":"codec.so","filename":artifact().filename,"sha256":hash(b"library")}),
            "Duplicate native artifact filename",
        ),
        (
            serde_json::json!({"path":"codec.so","url":"https://example.invalid/library","sha256":hash(b"library")}),
            "exactly one of path or url",
        ),
        (
            serde_json::json!({"url":"https://example.invalid/library","sha256":hash(b"library")}),
            "require a filename",
        ),
        (
            serde_json::json!({"url":"http://example.invalid/library","filename":"codec.so","sha256":hash(b"library")}),
            "HTTPS",
        ),
    ] {
        metadata["matchbox"]["artifacts"][TARGET]["libraries"] = serde_json::json!([library]);
        fs::write(&manifest, metadata.to_string()).unwrap();
        let output = cli(dir.path(), &["app.bxs", "--module", "module"]);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(error), "{stderr}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("loaded"));
    }
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
fn module_bifs_preserve_arguments_and_execution_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("module");
    fs::create_dir_all(module.join("bifs")).unwrap();
    fs::write(module.join("ModuleConfig.bx"), "class {}").unwrap();
    fs::write(
        module.join("bifs/Inspect.bx"),
        r#"
@BoxBIF(["inspectCall","inspectAlias"])
@BoxMember({string:{name:"inspectMember",objectArgument:"target"}})
class { function invoke(string prefix="default") { return arguments; } }
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/Bare.bx"),
        r#"
@BoxMember({string:{name:"bareArguments"}})
class { function invoke() { return arguments; } }
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app.bxs"),
        r#"
a = inspectAlias(prefix="named", extra={kept:42});
assert a.prefix == "named" && a.extra.kept == 42;
assert a.__functionName == "inspectAlias" && !a.__isMemberExecution;
a = inspectCall("first", "second");
assert a.prefix == "first" && a[2] == "second";
a = "receiver".inspectMember(extra=42,target="ignored");
assert a.target == "receiver" && a.prefix == "default" && a.extra == 42;
assert a.__functionName == "inspectMember" && a.__isMemberExecution;
a = "receiver".inspectMember("prefix");
assert a.prefix == "prefix" && a.target == "receiver";
a = "receiver".bareArguments(3,4);
assert a[1] == "receiver" && a[2] == 3 && a[3] == 4;
assert a.__isMemberExecution;
function ordinary(string declared="default") { return arguments; }
a = ordinary(extra=9);
assert a.declared == "default" && a.extra == 9;
a = ordinary("declared", 2, 3);
assert a.declared == "declared" && a[2] == 2 && a[3] == 3;
class Extra { function inspect(string value="default") { return arguments; } }
a = new Extra().inspect(extra=8);
assert a.value == "default" && a.extra == 8;
println("BIF invocation parity OK");
"#,
    )
    .unwrap();
    let output = cli(dir.path(), &["app.bxs", "--module", "module"]);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "BIF invocation parity OK"
    );
    success(&cli(
        dir.path(),
        &[
            "--build", "app.bxs", "--module", "module", "--output", "app.bxb",
        ],
    ));
    success(&cli(
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
    ));
    fs::remove_dir_all(&module).unwrap();
    fs::remove_file(dir.path().join("app.bxs")).unwrap();
    success(&cli(dir.path(), &["app.bxb"]));
    let output = Command::new(dir.path().join("application"))
        .current_dir(dir.path())
        .output()
        .unwrap();
    success(&output);
}

#[test]
fn interceptor_registration_roots_instances_until_unregister() {
    let mut vm = VM::new();
    let ast = matchbox_compiler::parser::parse(
        r#"
class Listener { function onGcProbe(data) { data.count += 1; } }
listener = new Listener();
"#,
        None,
    )
    .unwrap();
    let chunk = matchbox_compiler::compiler::Compiler::new("interceptor-gc.bxs")
        .compile(&ast, "")
        .unwrap();
    vm.interpret_sync(chunk).unwrap();
    let listener = vm.get_global("listener").unwrap();
    BxVM::append_interception_points(&mut vm, vec!["onGcProbe".into()]).unwrap();
    BxVM::register_interceptor(&mut vm, listener).unwrap();
    vm.insert_global("listener".into(), BxValue::new_null());
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    let data = BxVM::struct_new(&mut vm);
    BxVM::struct_set(&mut vm, data, "count", BxValue::new_number(0.0));
    BxVM::announce(&mut vm, "onGcProbe", BxValue::new_ptr(data)).unwrap();
    assert_eq!(BxVM::struct_get(&vm, data, "count").as_number(), 1.0);
    BxVM::unregister_interceptor(&mut vm, listener).unwrap();
    for _ in 0..16 {
        vm.collect_garbage_now();
    }
    assert!(vm.heap.get_opt(listener.as_gc_id().unwrap()).is_none());
}

#[test]
fn module_interceptors_receive_shared_data_and_short_circuit() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("module");
    fs::create_dir_all(module.join("bifs")).unwrap();
    fs::write(
        module.join("ModuleConfig.bx"),
        r#"
class {
    function configure() { customInterceptionPoints = ["onModuleProbe"]; settings = {step:2}; }
    function onModuleProbe(required struct data) { data.count += settings.step; }
    function onLoad() {
        assert boxRuntime.getInterceptorService() == interceptorService;
        assert interceptorService.hasInterceptionPoint("ONMODULEPROBE");
        data = {count:0};
        assert boxAnnounce(state="onModuleProbe",data=data);
        assert data.count == 2;
    }
}
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/InterceptorBus.bx"),
        "class { function invoke() { return interceptorService; } }",
    )
    .unwrap();
    fs::write(
        dir.path().join("app.bxs"),
        r#"
class Stop { function onModuleProbe(data) { data.count += 10; return true; } }
class Never { function onModuleProbe(data) { throw "chain did not stop"; } }
i = interceptorBus();
stopTarget = new Stop();
neverTarget = new Never();
i.register(stopTarget);
i.register(neverTarget);
data = {count:0};
i.announce("ONMODULEPROBE",data);
assert data.count == 12;
data.count = 0;
assert boxAnnounce("onModuleProbe",data);
assert data.count == 12;
function probeCaller() {
    boxAnnounce("onModuleProbe",{count:0});
    assert getFunctionCalledName() == "probeCaller";
}
probeCaller();
println("interceptors OK");
"#,
    )
    .unwrap();
    let output = cli(dir.path(), &["app.bxs", "--module", "module"]);
    success(&output);
    success(&cli(
        dir.path(),
        &[
            "--build", "app.bxs", "--module", "module", "--output", "app.bxb",
        ],
    ));
    success(&cli(
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
    ));
    fs::remove_dir_all(&module).unwrap();
    fs::remove_file(dir.path().join("app.bxs")).unwrap();
    success(&cli(dir.path(), &["app.bxb"]));
    success(
        &Command::new(dir.path().join("application"))
            .current_dir(dir.path())
            .output()
            .unwrap(),
    );
}

#[test]
fn parameterless_members_match_positional_and_named_jvm_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("module");
    fs::create_dir_all(module.join("bifs")).unwrap();
    fs::write(module.join("ModuleConfig.bx"), "class {}").unwrap();
    fs::write(
        module.join("bifs/NoParameters.bx"),
        r#"
@BoxMember({string:{name:"noParameters"}})
class { function invoke() { return "no parameters"; } }
"#,
    )
    .unwrap();
    fs::write(
        module.join("bifs/DocOnly.bx"),
        r#"
/**
 * @BoxBIF DocAlias
 * @BoxMember string
 */
class { function invoke(required string value) { return "doc:" & value; } }
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app.bxs"),
        r#"
assert noParameters() == "no parameters";
assert "value".noParameters() == "no parameters";
assert "value".noParameters(3, 4) == "no parameters";
assert (3).noParameters() == "no parameters";
assert docOnly("value") == "doc:value";
failed = false;
try { docAlias("value"); } catch (any e) { failed = true; }
assert failed;
failed = false;
try { "value".noParameters(extra=3); } catch (any e) {
    assert find("does not accept", e.message) > 0;
    failed = true;
}
assert failed;
noParameters = function() { return "replaced"; };
assert "value".noParameters() == "no parameters";
println("parameterless members OK");
"#,
    )
    .unwrap();
    let direct = cli(dir.path(), &["app.bxs", "--module", "module"]);
    success(&direct);
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout).trim(),
        "parameterless members OK"
    );
    success(&cli(
        dir.path(),
        &[
            "--build", "app.bxs", "--module", "module", "--output", "app.bxb",
        ],
    ));
    success(&cli(
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
    ));
    fs::remove_dir_all(module).unwrap();
    success(&cli(dir.path(), &["app.bxb"]));
    let standalone = Command::new(dir.path().join("application"))
        .current_dir(dir.path())
        .output()
        .unwrap();
    success(&standalone);
    assert_eq!(standalone.stdout, direct.stdout);
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
