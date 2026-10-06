//! Live module records and adapters to the services implemented by this VM.
use crate::{
    bundle::Module,
    types::{BxNativeObject, BxVM, BxValue, Tracer},
    vm::{VM, gc::GcObject},
};
use anyhow::{Result, bail};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

type Fields = Rc<RefCell<HashMap<String, BxValue>>>;

#[derive(Debug)]
struct ModuleRecord {
    fields: HashMap<String, BxValue>,
}
impl BxNativeObject for ModuleRecord {
    fn get_property(&self, name: &str) -> BxValue {
        self.fields
            .get(&name.to_lowercase())
            .copied()
            .unwrap_or(BxValue::new_null())
    }
    fn set_property(&mut self, name: &str, value: BxValue) {
        self.fields.insert(name.to_lowercase(), value);
    }
    fn call_method(
        &mut self,
        vm: &mut dyn BxVM,
        _id: usize,
        name: &str,
        args: &[BxValue],
    ) -> Result<BxValue, String> {
        if !args.is_empty() {
            return Err(format!("ModuleRecord.{name}() takes no arguments"));
        }
        match name.to_ascii_lowercase().as_str() {
            "asstruct" => {
                let id = vm.struct_new();
                for (key, value) in &self.fields {
                    vm.struct_set(id, key, *value);
                }
                Ok(BxValue::new_ptr(id))
            }
            "isenabled" => Ok(self.get_property("enabled")),
            "isactivated" => Ok(self.get_property("activated")),
            _ => Err(format!("Unsupported ModuleRecord method: {name}")),
        }
    }
    fn trace(&self, tracer: &mut dyn Tracer) {
        for value in self.fields.values() {
            tracer.mark(value);
        }
    }
}

pub(crate) fn record(
    vm: &mut VM,
    module: &Module,
    instance: BxValue,
    settings: BxValue,
) -> BxValue {
    let json = serde_json::json!({
        "name": module.name, "path": module.path, "physicalPath": module.path,
        "invocationPath": format!("bxModules.{}", module.mapping),
        "mapping": {"name": format!("/bxModules/{}", module.mapping), "path": module.path, "external": false},
        "dependencies": module.dependencies, "enabled": true, "activated": false,
        "bifs": [], "memberMethods": []
    });
    let mut fields = HashMap::new();
    for (key, value) in json.as_object().unwrap() {
        fields.insert(key.to_lowercase(), vm.json_to_bx(value.clone()));
    }
    for (name, default) in [
        ("version", "1.0.0"),
        ("author", ""),
        ("description", ""),
        ("webURL", ""),
    ] {
        let value = vm
            .instance_property(instance, name)
            .unwrap_or_else(|| BxValue::new_ptr(vm.string_new(default.into())));
        fields.insert(name.to_lowercase(), value);
    }
    fields.insert("settings".into(), settings);
    fields.insert("moduleconfig".into(), instance);
    BxValue::new_ptr(vm.native_object_new(Rc::new(RefCell::new(ModuleRecord { fields }))))
}

pub(crate) fn set_record(vm: &mut VM, record: BxValue, name: &str, value: BxValue) -> Result<()> {
    // get_mut also records old-to-young writes for generational GC.
    let GcObject::NativeObject(object) = vm.heap.get_mut(record.as_gc_id().unwrap()) else {
        bail!("Invalid module record");
    };
    object.borrow_mut().set_property(name, value);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Runtime,
    Functions,
    Modules,
    Interceptors,
    Log,
}
#[derive(Debug)]
struct Service {
    kind: Kind,
    fields: Fields,
}
impl BxNativeObject for Service {
    fn get_property(&self, name: &str) -> BxValue {
        self.fields
            .borrow()
            .get(&name.to_lowercase())
            .copied()
            .unwrap_or(BxValue::new_null())
    }
    fn set_property(&mut self, name: &str, value: BxValue) {
        self.fields.borrow_mut().insert(name.to_lowercase(), value);
    }
    fn call_method(
        &mut self,
        vm: &mut dyn BxVM,
        _id: usize,
        name: &str,
        args: &[BxValue],
    ) -> Result<BxValue, String> {
        let name = name.to_ascii_lowercase();
        let field = |name: &str| {
            self.fields
                .borrow()
                .get(name)
                .copied()
                .ok_or_else(|| format!("Service unavailable: {name}"))
        };
        match (self.kind, name.as_str()) {
            (Kind::Runtime, "getfunctionservice") if args.is_empty() => field("functionservice"),
            (Kind::Runtime, "getmoduleservice") if args.is_empty() => field("moduleservice"),
            (Kind::Runtime, "getinterceptorservice") if args.is_empty() => {
                field("interceptorservice")
            }
            (Kind::Interceptors, "register" | "unregister") if args.len() == 1 => {
                if name == "register" {
                    vm.register_interceptor(args[0])?;
                } else {
                    vm.unregister_interceptor(args[0])?;
                }
                field("interceptorservice")
            }
            (Kind::Interceptors, "announce") if (1..=2).contains(&args.len()) => {
                let state = vm.to_string(args[0]);
                let data = args
                    .get(1)
                    .copied()
                    .unwrap_or_else(|| BxValue::new_ptr(vm.struct_new()));
                vm.announce(&state, data)?;
                Ok(BxValue::new_null())
            }
            (Kind::Interceptors, "hasinterceptionpoint") if args.len() == 1 => {
                let state = vm.to_string(args[0]);
                Ok(BxValue::new_bool(
                    vm.interception_points()
                        .iter()
                        .any(|point| point.eq_ignore_ascii_case(&state)),
                ))
            }
            (Kind::Interceptors, "getinterceptionpoints" | "getinterceptionpointnames")
                if args.is_empty() =>
            {
                let id = vm.array_new();
                for point in vm.interception_points() {
                    let point = BxValue::new_ptr(vm.string_new(point));
                    vm.array_push(id, point);
                }
                Ok(BxValue::new_ptr(id))
            }
            (Kind::Interceptors, "appendinterceptionpoints") if !args.is_empty() => {
                let mut points = Vec::new();
                for value in args {
                    if vm.value_matches_type_name(*value, "array") {
                        let id = value.as_gc_id().unwrap();
                        points.extend(
                            (0..vm.array_len(id))
                                .map(|index| vm.to_string(vm.array_get(id, index))),
                        );
                    } else {
                        points.push(vm.to_string(*value));
                    }
                }
                vm.append_interception_points(points)?;
                field("interceptorservice")
            }
            (Kind::Functions, "hasglobalfunction") if args.len() == 1 => Ok(BxValue::new_bool(
                vm.has_global_function(&vm.to_string(args[0])),
            )),
            (Kind::Functions, "getglobalfunctionnames") if args.is_empty() => {
                let id = vm.array_new();
                for name in vm.global_function_names() {
                    let name = BxValue::new_ptr(vm.string_new(name));
                    vm.array_push(id, name);
                }
                Ok(BxValue::new_ptr(id))
            }
            (Kind::Modules, "getregistry") if args.is_empty() => field("registry"),
            (Kind::Modules, "getmodulerecord" | "hasmodule") if args.len() == 1 => {
                let registry = field("registry")?
                    .as_gc_id()
                    .ok_or("Invalid module registry")?;
                let name_arg = vm.to_string(args[0]);
                if name == "hasmodule" {
                    return Ok(BxValue::new_bool(vm.struct_key_exists(registry, &name_arg)));
                }
                if !vm.struct_key_exists(registry, &name_arg) {
                    return Err(format!("Module not loaded: {name_arg}"));
                }
                Ok(vm.struct_get(registry, &name_arg))
            }
            (Kind::Log, "trace" | "debug" | "info" | "warn" | "error") if args.len() == 1 => {
                eprintln!("[{name}] {}", vm.to_string(args[0]));
                Ok(BxValue::new_null())
            }
            (
                Kind::Log,
                "istraceenabled" | "isdebugenabled" | "isinfoenabled" | "iswarnenabled"
                | "iserrorenabled",
            ) if args.is_empty() => Ok(BxValue::new_bool(true)),
            _ => Err(format!(
                "Unsupported MatchBox {:?} service method or arguments: {name}",
                self.kind
            )),
        }
    }
    fn trace(&self, tracer: &mut dyn Tracer) {
        for value in self.fields.borrow().values() {
            tracer.mark(value);
        }
    }
}

pub(crate) struct Services {
    fields: Fields,
}
impl Services {
    pub fn new(vm: &mut VM) -> Self {
        let registry = BxValue::new_ptr(vm.struct_new());
        let fields = Rc::new(RefCell::new(HashMap::from([("registry".into(), registry)])));
        for (name, kind) in [
            ("boxruntime", Kind::Runtime),
            ("functionservice", Kind::Functions),
            ("moduleservice", Kind::Modules),
            ("interceptorservice", Kind::Interceptors),
            ("log", Kind::Log),
        ] {
            let value = BxValue::new_ptr(vm.native_object_new(Rc::new(RefCell::new(Service {
                kind,
                fields: fields.clone(),
            }))));
            fields.borrow_mut().insert(name.into(), value);
            vm.insert_global(format!("__mb_{name}"), value);
        }
        Self { fields }
    }
    pub fn register(&self, vm: &mut VM, name: &str, record: BxValue) {
        let id = self.fields.borrow()["registry"].as_gc_id().unwrap();
        vm.struct_set(id, name, record);
    }
    pub fn inject(&self, vm: &mut VM, instance: BxValue, record: BxValue, bif: bool) -> Result<()> {
        vm.set_instance_variable(instance, "moduleRecord", record)?;
        for name in ["boxruntime", "log", "interceptorservice"] {
            vm.set_instance_variable(instance, name, self.fields.borrow()[name])?;
        }
        if bif {
            vm.set_instance_variable(
                instance,
                "functionService",
                self.fields.borrow()["functionservice"],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chunk;

    #[test]
    fn service_registry_roots_records_and_tracks_new_settings_through_gc() {
        let mut vm = VM::new();
        let services = Services::new(&mut vm);
        let module = Module {
            name: "gc-test".into(),
            path: "/module".into(),
            mapping: "gc-test".into(),
            dependencies: vec![],
            instance: "config".into(),
            descriptor: Chunk::default(),
            bifs: Chunk::default(),
            bif_registrations: vec![],
            settings: serde_json::json!({}),
            artifact: None,
        };
        let settings = BxValue::new_ptr(vm.struct_new());
        let record = record(&mut vm, &module, BxValue::new_null(), settings);
        services.register(&mut vm, "gc-test", record);
        for _ in 0..16 {
            vm.collect_garbage_now();
        }
        let settings_id = vm.struct_new();
        let text = BxValue::new_ptr(vm.string_new("survived".into()));
        vm.struct_set(settings_id, "text", text);
        set_record(&mut vm, record, "settings", BxValue::new_ptr(settings_id)).unwrap();
        for _ in 0..16 {
            vm.collect_garbage_now();
        }
        let snapshot = vm
            .call_method_value(record, "asStruct", vec![])
            .unwrap()
            .as_gc_id()
            .unwrap();
        let settings = vm.struct_get(snapshot, "settings").as_gc_id().unwrap();
        assert_eq!(vm.to_string(vm.struct_get(settings, "text")), "survived");
        let module_service = services.fields.borrow()["moduleservice"];
        let name = BxValue::new_ptr(vm.string_new("GC-TEST".into()));
        assert_eq!(
            vm.call_method_value(module_service, "getModuleRecord", vec![name])
                .unwrap(),
            record
        );
    }
}
