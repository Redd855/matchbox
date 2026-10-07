//! Shared function scopes. Closures retain the declaring structs, not stack snapshots.
use super::{VM, gc::GcObject};
use crate::types::{BxVM, BxValue};

impl VM {
    fn arguments_scope(&self, fiber: usize) -> Option<BxValue> {
        let frame = self.fibers[fiber].frames.last()?;
        (frame.function.arity as usize == frame.function.params.len() + 1)
            .then(|| self.fibers[fiber].stack[frame.stack_base + frame.function.params.len()])
    }

    pub(super) fn local_scope(&mut self, fiber: usize) -> BxValue {
        if let Some(scope) = self.fibers[fiber].frames.last().unwrap().local_scope {
            return scope;
        }
        let scope = BxValue::new_ptr(self.struct_new());
        self.fibers[fiber].frames.last_mut().unwrap().local_scope = Some(scope);
        scope
    }

    pub(super) fn bind_local(&mut self, fiber: usize, slot: usize, name: String) {
        let frame = self.fibers[fiber].frames.last().unwrap();
        let value = self.fibers[fiber].stack[frame.stack_base + slot];
        let scope = self.local_scope(fiber).as_gc_id().unwrap();
        self.struct_set(scope, &name, value);
        self.fibers[fiber]
            .frames
            .last_mut()
            .unwrap()
            .local_names
            .insert(slot, name);
    }

    fn slot_scope(&self, fiber: usize, slot: usize) -> Option<(usize, &str)> {
        let frame = self.fibers[fiber].frames.last().unwrap();
        if let Some(name) = frame.local_names.get(&slot) {
            return Some((frame.local_scope?.as_gc_id()?, name));
        }
        let name = frame.function.params.get(slot)?;
        Some((self.arguments_scope(fiber)?.as_gc_id()?, name))
    }

    pub(super) fn local_value(&self, fiber: usize, slot: usize) -> BxValue {
        let value = self
            .slot_scope(fiber, slot)
            .and_then(|(scope, name)| self.scope_value(BxValue::new_ptr(scope), name));
        if let Some(value) = value {
            return value;
        }
        let frame = self.fibers[fiber].frames.last().unwrap();
        self.fibers[fiber].stack[frame.stack_base + slot]
    }

    pub(super) fn set_local_value(&mut self, fiber: usize, slot: usize, value: BxValue) {
        if let Some((scope, name)) = self.slot_scope(fiber, slot) {
            let name = name.to_owned();
            self.struct_set(scope, &name, value);
        }
        let base = self.fibers[fiber].frames.last().unwrap().stack_base;
        self.fibers[fiber].stack[base + slot] = value;
    }

    #[cfg(feature = "jit")]
    pub(super) fn sync_jit_locals(&mut self, fiber: usize, from_stack: bool) {
        let frame = self.fibers[fiber].frames.last().unwrap();
        let base = frame.stack_base;
        let mut slots: Vec<_> = frame
            .local_names
            .keys()
            .copied()
            .chain(0..frame.function.params.len())
            .filter(|slot| base + slot < self.fibers[fiber].stack.len())
            .collect();
        slots.sort_unstable();
        for slot in slots {
            if from_stack {
                let value = self.fibers[fiber].stack[base + slot];
                self.set_local_value(fiber, slot, value);
            } else {
                let value = self.local_value(fiber, slot);
                self.fibers[fiber].stack[base + slot] = value;
            }
        }
    }

    pub(super) fn truncate_scope_stack(&mut self, fiber: usize, length: usize) {
        let frame = self.fibers[fiber].frames.last_mut().unwrap();
        frame
            .local_names
            .retain(|slot, _| frame.stack_base + slot < length);
        self.fibers[fiber].stack.truncate(length);
    }

    fn scope_value(&self, scope: BxValue, name: &str) -> Option<BxValue> {
        let id = scope.as_gc_id()?;
        let GcObject::Struct(scope) = self.heap.get(id) else {
            return None;
        };
        let key = self.struct_key_lookup_id(id, name)?;
        self.shapes
            .get_index(scope.shape_id, key)
            .map(|slot| scope.properties[slot as usize])
    }

    pub(super) fn function_scope_value(&mut self, fiber: usize, name: &str) -> Option<BxValue> {
        if name == "local" && self.arguments_scope(fiber).is_some() {
            return Some(self.local_scope(fiber));
        }
        if name == "arguments" {
            return self.arguments_scope(fiber);
        }
        self.function_scope_for(fiber, name)
            .and_then(|scope| self.scope_value(scope, name))
    }

    fn function_scope_for(&self, fiber: usize, name: &str) -> Option<BxValue> {
        let frame = self.fibers[fiber].frames.last().unwrap();
        frame
            .local_scope
            .into_iter()
            .chain(self.arguments_scope(fiber))
            .chain(frame.function.captured_scopes.iter().copied())
            .find(|scope| self.scope_value(*scope, name).is_some())
    }

    pub(super) fn assign_function_scope(
        &mut self,
        fiber: usize,
        name: &str,
        value: BxValue,
    ) -> bool {
        if let Some(scope) = self.function_scope_for(fiber, name) {
            self.struct_set(scope.as_gc_id().unwrap(), name, value);
            return true;
        }
        false
    }

    pub(super) fn capture_scopes(&mut self, fiber: usize) -> Vec<BxValue> {
        let Some(arguments) = self.arguments_scope(fiber) else {
            return Vec::new();
        };
        let local = self.local_scope(fiber);
        let mut scopes = vec![local, arguments];
        scopes.extend(
            self.fibers[fiber]
                .frames
                .last()
                .unwrap()
                .function
                .captured_scopes
                .iter()
                .copied(),
        );
        scopes
    }
}
