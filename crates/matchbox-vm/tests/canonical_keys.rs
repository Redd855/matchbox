use matchbox_vm::{
    types::{BxVM, BxValue},
    vm::VM,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

// Per-thread allocation accounting: measure public VM calls, not test-runner threads.
thread_local! { static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) }; }
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
