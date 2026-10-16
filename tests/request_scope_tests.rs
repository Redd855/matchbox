use matchbox_compiler::{compiler::Compiler, parser};
use matchbox_vm::{types::{BxVM, BxValue, NativeFutureValue}, vm::{VM, HostFutureState}};

fn execute(vm: &mut VM, source: &str) -> anyhow::Result<BxValue> {
    let ast = parser::parse(source, Some("request_scope.bxs"))?;
    let chunk = Compiler::new("request_scope.bxs").compile(&ast, source)?;
    vm.interpret_chunk_borrowed_current_task(&chunk)
}

#[test]
fn isolated_requests_restore_globals_after_errors_and_panic_and_keep_host_roots_alive() {
    let mut vm = VM::new();
    let original = BxValue::new_ptr(vm.string_new("host-value".into()));
    vm.insert_global("hostValue".into(), original);
    let error = vm.with_isolated_globals::<()>(|vm| {
        execute(vm, "hostValue='changed'; temporary='private';")?;
        vm.collect_garbage_now();
        anyhow::bail!("request failed")
    });
    assert!(error.is_err());
    assert!(vm.get_global("temporary").is_none());
    assert_eq!(vm.to_string(vm.get_global("hostValue").unwrap()), "host-value");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        vm.with_isolated_globals::<()>(|vm| {
            vm.insert_global("private".into(), BxValue::new_bool(true));
            panic!("expected request panic");
        }).unwrap();
    }));
    assert!(panic.is_err());
    assert!(vm.get_global("private").is_none());
    vm.with_isolated_globals(|vm| { execute(vm, "writeOutput('recovered');")?; Ok(()) }).unwrap();
}

#[test]
fn retained_functions_and_objects_survive_gc_and_rebind_current_globals() {
    let mut vm = VM::new();
    let function = vm.with_isolated_globals(|vm| {
        execute(vm, "value=11; function readValue() { return value; }")?;
        assert_eq!(vm.call_function("readValue", vec![], None)?.as_number(), 11.0);
        vm.retain_value(vm.get_global("readValue").unwrap()).map_err(anyhow::Error::msg)
    }).unwrap();
    vm.collect_garbage_now();
    vm.with_isolated_globals(|vm| {
        vm.insert_global("unrelated".into(), BxValue::new_bool(true));
        vm.insert_global("value".into(), BxValue::new_number(22.0));
        let function = vm.read_retained_value(&function).map_err(anyhow::Error::msg)?;
        assert_eq!(vm.call_function_value(function, vec![], None)?.as_number(), 22.0);
        Ok(())
    }).unwrap();
}

#[test]
fn detached_fibers_are_rejected_and_cannot_run_in_a_later_request() {
    let mut vm = VM::new();
    let result = vm.with_isolated_globals(|vm| {
        execute(vm, "runAsync(function() { sleep(10000); leaked=true; });")?;
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("Detached"));
    assert!(vm.fibers.is_empty());
    vm.with_isolated_globals(|vm| { assert!(vm.get_global("leaked").is_none()); Ok(()) }).unwrap();
}

#[test]
fn cancelled_native_futures_stay_rooted_until_late_completion_is_drained() {
    let mut vm = VM::new();
    let mut handle = None;
    let mut retained = None;
    let result = vm.with_isolated_globals(|vm| {
        let sender = vm.native_future_new();
        retained = Some(vm.retain_value(sender.future()).map_err(anyhow::Error::msg)?);
        handle = Some(sender);
        Ok(())
    });
    assert!(result.is_err());
    vm.collect_garbage_now();
    handle.take().unwrap().resolve(NativeFutureValue::String("late".into())).unwrap();
    vm.with_isolated_globals(|vm| {
        let future = vm.read_retained_value(retained.as_ref().unwrap()).map_err(anyhow::Error::msg)?;
        assert!(matches!(vm.future_state(future)?, HostFutureState::Failed(_)));
        Ok(())
    }).unwrap();
}
