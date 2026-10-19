use matchbox_compiler::{compiler::Compiler, parser};
use matchbox_vm::{Chunk, types::{BxVM, BxValue}, vm::VM};

fn compile(source: &str) -> Chunk {
    let ast = parser::parse(source, Some("namespace_epoch.bxs")).unwrap();
    Compiler::new("namespace_epoch.bxs").compile(&ast, source).unwrap()
}

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn namespace_epoch_work_start() { std::hint::black_box(()); }

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn namespace_epoch_work_end() { std::hint::black_box(()); }

#[test]
fn retained_function_rejects_stale_bindings_across_restoration() {
    let source = compile("function readCurrent() { return new Current().marker(); }");
    let mut vm = VM::new();
    vm.interpret_chunk_with_receiver(&source, None).unwrap();
    let reader = vm.retain_value(vm.get_global("readCurrent").unwrap()).unwrap();
    // Warm the retained function against a request-only binding, not a fresh function per request.
    vm.with_isolated_globals(|vm| {
        vm.interpret_chunk_with_receiver(&compile("class Current { function marker() { return 11; } }"), None)?;
        assert_eq!(vm.call_function_value(vm.read_retained_value(&reader).unwrap(), vec![], None)?.as_number(), 11.0);
        Ok(())
    }).unwrap();

    namespace_epoch_work_start();
    for pass in 0..20 {
        let result = vm.with_isolated_globals(|vm| {
            // The old slot contains a valid but wrong class: bounds checks alone cannot pass.
            vm.interpret_chunk_with_receiver(&compile("class Padding { function marker() { return -999; } }"), None)?;
            if pass % 3 != 0 {
                vm.interpret_chunk_with_receiver(&compile(&format!("class Current {{ function marker() {{ return {}; }} }}", 100 + pass)), None)?;
            }
            let reader = vm.read_retained_value(&reader).unwrap();
            let value = vm.call_function_value(reader, vec![], None)?;
            assert_eq!(value.as_number(), 100.0 + pass as f64);
            // A value-only update keeps the same binding, and must not cache the old class.
            vm.interpret_chunk_with_receiver(&compile(&format!("class Current {{ function marker() {{ return {}; }} }}", 200 + pass)), None)?;
            assert_eq!(vm.call_function_value(reader, vec![], None)?.as_number(), 200.0 + pass as f64);
            Ok(())
        });
        if pass % 3 == 0 { assert!(result.is_err(), "missing class must not read the recycled slot"); }
        else { result.unwrap(); }
        assert!(vm.get_global("Current").is_none());
    }
    namespace_epoch_work_end();
}
