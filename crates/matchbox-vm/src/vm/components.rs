//! Module components use ordinary, GC-traced instances and registration records.
use super::{VM, gc::GcObject};
use crate::{
    bundle::Component,
    types::{BxNativeObject, BxVM, BxValue, NativeCallback},
};
use anyhow::{Context, Result, bail};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug)]
struct DefaultBodyResult;
impl BxNativeObject for DefaultBodyResult {
    fn native_data(&self, kind: &str) -> Option<*mut std::ffi::c_void> {
        (kind == "matchbox.component.body_result")
            .then_some(self as *const Self as *mut std::ffi::c_void)
    }
    fn get_property(&self, name: &str) -> BxValue {
        if name.eq_ignore_ascii_case("resultType") {
            BxValue::new_number(0.0)
        } else {
            BxValue::new_null()
        }
    }
    fn set_property(&mut self, _name: &str, _value: BxValue) {}
    fn call_method(
        &mut self,
        _vm: &mut dyn BxVM,
        _id: usize,
        name: &str,
        args: &[BxValue],
    ) -> Result<BxValue, String> {
        let name = name.to_ascii_lowercase();
        if !args.is_empty()
            && !(matches!(name.as_str(), "isbreak" | "iscontinue") && args.len() == 1)
        {
            return Err("Invalid default body result arguments".into());
        }
        match name.as_str() {
            "isearlyexit" | "isbreak" | "iscontinue" | "isreturn" => Ok(BxValue::new_bool(false)),
            "returnvalue" | "label" => Ok(BxValue::new_null()),
            "resulttype" => Ok(BxValue::new_number(0.0)),
            _ => Err(format!("Unsupported component body result method: {name}")),
        }
    }
}
fn default_body_result(vm: &mut dyn BxVM) -> BxValue {
    BxValue::new_ptr(vm.native_object_new(Rc::new(RefCell::new(DefaultBodyResult))))
}

const COMPONENTS: &str = "__mb_components";

pub(crate) fn inject_helpers(vm: &mut VM, target: BxValue, name: &str) -> Result<()> {
    let name = name.to_owned();
    let callback = NativeCallback {
        parameters: vec![],
        callback: Rc::new(move |vm, args| {
            if !args.is_empty() {
                return Err("getName() takes no arguments".into());
            }
            Ok(BxValue::new_ptr(vm.string_new(name.clone())))
        }),
    };
    let value = BxValue::new_ptr(vm.heap.alloc(GcObject::NativeCallback(callback)));
    vm.set_instance_variable(target, "getName", value)?;
    for (name, callback) in [
        (
            "newBuffer",
            NativeCallback {
                parameters: vec![],
                callback: Rc::new(|vm, args| {
                    if !args.is_empty() {
                        return Err("newBuffer() takes no arguments".into());
                    }
                    Ok(crate::bifs::string_buffer_new(vm))
                }),
            },
        ),
        (
            "newBuilder",
            NativeCallback {
                parameters: vec![],
                callback: Rc::new(|vm, args| {
                    if !args.is_empty() {
                        return Err("newBuilder() takes no arguments".into());
                    }
                    crate::bifs::string_builder_new_bif(vm, &[])
                }),
            },
        ),
        (
            "processBody",
            NativeCallback {
                parameters: vec!["context".into(), "body".into(), "buffer".into()],
                callback: Rc::new(process_body),
            },
        ),
    ] {
        let value = BxValue::new_ptr(vm.heap.alloc(GcObject::NativeCallback(callback)));
        vm.set_instance_variable(target, name, value)?;
    }
    Ok(())
}

fn process_body(vm: &mut dyn BxVM, args: &[BxValue]) -> Result<BxValue, String> {
    if args.len() != 3 {
        return Err("processBody() requires context, body and buffer".into());
    }
    if args[1].is_null() {
        return Ok(default_body_result(vm));
    }
    let chunk = vm
        .current_chunk()
        .ok_or("Component body context unavailable")?;
    let capture = vm
        .native_object_data(args[2], "matchbox.component.buffer")
        .is_some();
    if capture {
        vm.begin_output_capture();
    }
    let result = vm.call_function_by_value(&args[1], vec![args[0]], chunk);
    if capture {
        let output = vm.end_output_capture().unwrap_or_default();
        let text = BxValue::new_ptr(vm.string_new(output.clone()));
        vm.native_object_call_method(args[2].as_gc_id().unwrap(), "append", &[text])?;
        if result.is_err() {
            vm.write_output(&output);
        }
    }
    let result = result?;
    if result.is_null() {
        return Ok(default_body_result(vm));
    }
    if vm
        .native_object_data(result, "matchbox.component.body_result")
        .is_none()
    {
        return Err("Component body returned an unsupported early-exit result".into());
    }
    Ok(result)
}

impl VM {
    pub(crate) fn register_component(
        &mut self,
        target: BxValue,
        component: &Component,
    ) -> Result<()> {
        if !self.instance_has_method(target, "invoke") {
            bail!("Module component {} needs invoke()", component.class);
        }
        let registry = self
            .get_global(COMPONENTS)
            .unwrap_or_else(|| self.insert_empty_struct_global(COMPONENTS))
            .as_gc_id()
            .unwrap();
        let record = self.struct_new();
        let name = BxValue::new_ptr(self.string_new(component.class.clone()));
        self.struct_set(record, "name", name);
        self.struct_set(record, "target", target);
        self.struct_set(
            record,
            "allowsBody",
            BxValue::new_bool(component.allows_body || component.requires_body),
        );
        self.struct_set(
            record,
            "requiresBody",
            BxValue::new_bool(component.requires_body),
        );
        for alias in &component.aliases {
            self.struct_set(registry, alias, BxValue::new_ptr(record));
        }
        Ok(())
    }

    pub(crate) fn component_names(&self) -> Vec<String> {
        self.get_global(COMPONENTS)
            .and_then(|value| value.as_gc_id())
            .map(|id| self.struct_key_array(id))
            .unwrap_or_default()
    }

    pub(crate) fn invoke_component(
        &mut self,
        name: &str,
        attributes: BxValue,
        body: BxValue,
    ) -> Result<BxValue> {
        if !self.is_struct_value(attributes) {
            bail!("Component attributes must be a struct");
        }
        let registry = self
            .get_global(COMPONENTS)
            .and_then(|value| value.as_gc_id())
            .with_context(|| format!("Component not registered: {name}"))?;
        let record = self
            .struct_get(registry, name)
            .as_gc_id()
            .with_context(|| format!("Component not registered: {name}"))?;
        if body.is_null() && self.struct_get(record, "requiresBody").as_bool() {
            bail!("Component {name} requires a body");
        }
        if !body.is_null() && !self.struct_get(record, "allowsBody").as_bool() {
            bail!("Component {name} does not allow a body");
        }
        let body = if body.is_null() {
            let callback = NativeCallback {
                parameters: vec!["context".into()],
                callback: Rc::new(|vm, _| Ok(default_body_result(vm))),
            };
            BxValue::new_ptr(self.heap.alloc(GcObject::NativeCallback(callback)))
        } else {
            body
        };
        let target = self.struct_get(record, "target");
        let context = self.call_function("getBoxContext", vec![], None)?;
        let state = self.struct_new();
        let registered_name = self.struct_get(record, "name");
        self.struct_set(state, "name", registered_name);
        self.struct_set(state, "attributes", attributes);
        let collection = BxValue::new_ptr(self.struct_new());
        self.struct_set(state, "dataCollection", collection);
        let result = self.call_method_value_named(
            target,
            "invoke",
            vec![context, attributes, body, BxValue::new_ptr(state)],
            vec![
                "context".into(),
                "attributes".into(),
                "body".into(),
                "executionState".into(),
            ],
        )?;
        if !result.is_null()
            && !result.as_gc_id().is_some_and(|id| match self.heap.get(id) {
                GcObject::NativeObject(object) => object
                    .borrow()
                    .native_data("matchbox.component.body_result")
                    .is_some(),
                _ => false,
            })
        {
            bail!("Module component invoke() must return a body result or null");
        }
        Ok(BxValue::new_null())
    }
}
