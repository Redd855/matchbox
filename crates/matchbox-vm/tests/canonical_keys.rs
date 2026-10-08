use matchbox_vm::{
    types::{BxCompiledFunction, BxVM, BxValue, Constant, FunctionKind, box_string::BoxString},
    vm::{
        VM,
        chunk::{Chunk, ChunkProgram},
        gc::GcObject,
        opcode::op,
    },
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

// Per-thread allocation accounting: measure public VM calls, not test-runner threads.
thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    static ALLOCATION_BYTES: Cell<Option<usize>> = const { Cell::new(None) };
}
struct CountedSystem;
#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| {
            if let Some(value) = count.get() {
                count.set(Some(value + 1));
            }
        });
        let _ = ALLOCATION_BYTES.try_with(|count| {
            if let Some(value) = count.get() {
                count.set(Some(value + layout.size()));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[test]
fn existing_canonical_struct_reads_do_not_allocate_temporary_keys() {
    let mut vm = VM::new();
    let data = vm.struct_new();
    let value = BxValue::new_int(42);
    for key in ["name", "élève中😀"] {
        vm.struct_set(data, key, value);
    }
    ALLOCATIONS.with(|count| count.set(Some(0)));
    for _ in 0..100 {
        for key in ["name", "élève中😀"] {
            assert_eq!(vm.struct_get(data, key), value);
            assert!(vm.struct_key_exists(data, key));
        }
    }
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(
        allocations, 0,
        "Existing canonical reads should not allocate temporary keys"
    );
}

#[test]
fn existing_canonical_struct_writes_do_not_allocate_temporary_keys() {
    let mut vm = VM::new();
    let data = vm.struct_new();
    for key in ["name", "élève中😀"] {
        vm.struct_set(data, key, BxValue::new_int(0));
    }
    ALLOCATIONS.with(|count| count.set(Some(0)));
    for number in 1..=100 {
        for key in ["name", "élève中😀"] {
            let value = BxValue::new_int(number);
            vm.struct_set(data, key, value);
            assert_eq!(vm.struct_get(data, key), value);
        }
    }
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(
        allocations, 0,
        "Existing canonical writes should not allocate temporary keys"
    );
}

#[test]
fn identifier_bytecode_reads_do_not_allocate_per_instruction() {
    let mut allocations = Vec::new();
    for repeats in [1, 100] {
        let mut vm = VM::new();
        vm.insert_global("knownoperand".into(), BxValue::new_int(42));
        let mut chunk = Chunk::new("operand-read.bxs");
        let name = chunk.add_constant(Constant::String(BoxString::new("knownoperand")));
        for _ in 1..repeats {
            chunk.emit1(op::GET_GLOBAL, name, 1);
            chunk.emit0(op::POP, 1);
        }
        chunk.emit1(op::GET_GLOBAL, name, 1);
        chunk.emit0(op::RETURN, 1);
        ALLOCATIONS.with(|count| count.set(Some(0)));
        let result = vm.interpret_chunk_borrowed(&chunk).unwrap();
        let count = ALLOCATIONS.with(|count| count.replace(None).unwrap());
        assert_eq!(result, BxValue::new_int(42));
        allocations.push(count);
    }
    eprintln!("Identifier allocations, 1/100 reads: {allocations:?}");
    // Interpreter startup may allocate; reading the same metadata must not add
    // a temporary owned string for every cold instruction/cache site.
    assert!(
        allocations[1] <= allocations[0] + 8,
        "Repeated identifier reads allocate: {allocations:?}"
    );
}

#[test]
fn warmed_function_calls_do_not_reallocate_instruction_sized_buffers() {
    let mut bytes = Vec::new();
    for padding in [0, 2000] {
        let mut vm = VM::new();
        let mut body = Chunk::new("function-buffer.bxs");
        let null = body.add_constant(Constant::Null);
        for _ in 0..padding {
            body.emit1(op::CONSTANT, null, 1);
            body.emit0(op::POP, 1);
        }
        let answer = body.add_constant(Constant::Number(42.0));
        body.emit1(op::CONSTANT, answer, 1);
        body.emit0(op::RETURN, 1);
        let function = BxCompiledFunction {
            name: "answer".into(),
            kind: FunctionKind::Udf,
            arity: 0,
            min_arity: 0,
            params: Vec::new(),
            bif: None,
            modifiers: Default::default(),
            captured_receiver: None,
            captured_scopes: Vec::new(),
            chunk: body,
        };
        let mut script = Chunk::new("function-owner.bxs");
        let function = script.add_constant(Constant::CompiledFunction(function));
        script.emit1(op::CONSTANT, function, 1);
        script.emit0(op::RETURN, 1);
        let function = vm.interpret(script).unwrap();
        assert_eq!(
            vm.call_function_value(function, Vec::new(), None)
                .unwrap()
                .as_number(),
            42.0
        );
        ALLOCATION_BYTES.with(|count| count.set(Some(0)));
        for _ in 0..10 {
            assert_eq!(
                vm.call_function_value(function, Vec::new(), None)
                    .unwrap()
                    .as_number(),
                42.0
            );
        }
        bytes.push(ALLOCATION_BYTES.with(|count| count.replace(None).unwrap()));
    }
    eprintln!("Warm call allocation bytes, 0/2000 padding pairs: {bytes:?}");
    assert!(
        bytes[1] <= bytes[0] + 8192,
        "Warm calls reallocate buffers sized by function instructions: {bytes:?}"
    );
}

#[test]
fn identifier_operands_preserve_unicode_case_folding_and_rope_text() {
    let key = BoxString::new("Élève中😀_identifier_").concat(&BoxString::new("long_tail"));
    let mut vm = VM::new();
    vm.insert_global(
        "élève中😀_identifier_long_tail".into(),
        BxValue::new_int(42),
    );
    let mut script = Chunk::new("unicode-operand.bxs");
    let key = script.add_constant(Constant::String(key));
    script.emit1(op::GET_GLOBAL, key, 1);
    script.emit0(op::RETURN, 1);
    assert_eq!(vm.interpret(script).unwrap().as_int(), 42);
}

#[test]
fn shared_script_programs_do_not_share_vm_global_indices() {
    let mut script = Chunk::new("shared-script.bxs");
    let key = script.add_constant(Constant::String(BoxString::new("answer")));
    script.emit1(op::GET_GLOBAL, key, 1);
    script.emit0(op::RETURN, 1);
    let mut first = VM::new();
    first.insert_global("answer".into(), BxValue::new_int(42));
    assert_eq!(first.interpret(script.clone()).unwrap().as_int(), 42);
    let mut second = VM::new();
    second.insert_global("padding".into(), BxValue::new_int(-1));
    second.insert_global("answer".into(), BxValue::new_int(64));
    assert_eq!(second.interpret(script).unwrap().as_int(), 64);
}

#[test]
fn immutable_programs_remain_thread_shareable() {
    fn thread_shareable<T: Send + Sync>() {}
    thread_shareable::<ChunkProgram>();
}

#[test]
fn copied_function_metadata_does_not_reuse_another_vms_global_indices() {
    let mut first = VM::new();
    first.insert_global("answer".into(), BxValue::new_int(42));
    let mut body = Chunk::new("vm-local-cache.bxs");
    let key = body.add_constant(Constant::String(BoxString::new("answer")));
    body.emit1(op::GET_GLOBAL, key, 1);
    body.emit0(op::RETURN, 1);
    let function = BxCompiledFunction {
        name: "readAnswer".into(),
        kind: FunctionKind::Udf,
        arity: 0,
        min_arity: 0,
        params: Vec::new(),
        bif: None,
        modifiers: Default::default(),
        captured_receiver: None,
        captured_scopes: Vec::new(),
        chunk: body,
    };
    let mut script = Chunk::new("cache-transfer.bxs");
    let slot = script.add_constant(Constant::CompiledFunction(function));
    script.emit1(op::CONSTANT, slot, 1);
    script.emit0(op::RETURN, 1);
    let value = first.interpret_chunk_borrowed(&script).unwrap();
    assert_eq!(
        first
            .call_function_value(value, Vec::new(), None)
            .unwrap()
            .as_int(),
        42
    );
    // Stage a consumer's copy of actual warmed metadata, not a cache assertion.
    let GcObject::CompiledFunction(function) = first.heap.get(value.as_gc_id().unwrap()) else {
        panic!("Expected compiled function");
    };
    let mut transferred = Chunk::new("transferred-function.bxs");
    let slot = transferred.add_constant(Constant::CompiledFunction((**function).clone()));
    transferred.emit1(op::CONSTANT, slot, 1);
    transferred.emit0(op::RETURN, 1);
    let mut second = VM::new();
    second.insert_global("padding".into(), BxValue::new_int(-1));
    second.insert_global("answer".into(), BxValue::new_int(64));
    let value = second.interpret(transferred).unwrap();
    assert_eq!(
        second
            .call_function_value(value, Vec::new(), None)
            .unwrap()
            .as_int(),
        64
    );
}
