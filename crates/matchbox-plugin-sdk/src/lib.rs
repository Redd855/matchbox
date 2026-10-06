//! Stable desktop plugin ABI. No VM implementation or Rust-owned objects cross this boundary.
//!
//! All callbacks run synchronously on the VM thread. Value handles are nonzero, opaque,
//! and valid only for the current invocation. Plugins must not retain handles or context
//! pointers. Object payloads belong to the plugin and are released by its drop callback.
//! A capability's layout and semantics are immutable; incompatible changes use a new name.
//! Trusted plugins must supply valid pointers and must never unwind across these functions.
use std::ffi::c_void;

pub const ABI_MAJOR: u32 = 1;
pub const VALUES_V1: &str = "matchbox.values.v1";
pub type Value = u64;
pub type Context = *mut c_void;
pub type Invoke = unsafe extern "C" fn(
    Context,
    *const ValuesV1,
    *mut c_void,
    *const Value,
    usize,
    *mut Value,
) -> i32;
pub type DropObject = unsafe extern "C" fn(*mut c_void);
pub type Init = unsafe extern "C" fn(*const HostV1, Context) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Text {
    pub ptr: *const u8,
    pub len: usize,
}
impl Text {
    pub fn new(s: &str) -> Self {
        Self {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// JSON registration metadata: {"name":"...","arguments":[{"name":"...",
/// "required":true,"default":null}]}. Defaults are null, numbers, strings, or bools.
#[repr(C)]
/// Borrowed only during initialization. Returned capability tables remain valid for
/// the host process lifetime; registration/error context pointers must not be retained.
pub struct HostV1 {
    pub abi_major: u32,
    pub capability: unsafe extern "C" fn(Text) -> *const c_void,
    pub register: unsafe extern "C" fn(Context, Text, Invoke) -> i32,
    pub error: unsafe extern "C" fn(Context, Text),
}

#[repr(C)]
pub struct ValuesV1 {
    /// Returned UTF-8 remains valid until this invocation ends.
    pub string_get: unsafe extern "C" fn(Context, Value, *mut Text) -> i32,
    pub number_get: unsafe extern "C" fn(Context, Value, *mut f64) -> i32,
    pub number_new: unsafe extern "C" fn(Context, f64, *mut Value) -> i32,
    pub string_new: unsafe extern "C" fn(Context, Text, *mut Value) -> i32,
    pub bool_new: unsafe extern "C" fn(Context, bool, *mut Value) -> i32,
    pub null_new: unsafe extern "C" fn(Context, *mut Value) -> i32,
    pub struct_new: unsafe extern "C" fn(Context, *mut Value) -> i32,
    pub struct_set: unsafe extern "C" fn(Context, Value, Text, Value) -> i32,
    /// Ownership transfers only on success. method metadata: {"info":"imageInfo"}.
    pub object_new:
        unsafe extern "C" fn(Context, Text, *mut c_void, DropObject, Text, *mut Value) -> i32,
    pub object_data: unsafe extern "C" fn(Context, Value, Text, *mut *mut c_void) -> i32,
    pub error: unsafe extern "C" fn(Context, Text),
}

/// Ergonomic, call-scoped wrapper. Construct only from the host's invocation callback.
pub struct Call<'a> {
    context: Context,
    api: &'a ValuesV1,
    args: &'a [Value],
}
impl<'a> Call<'a> {
    /// # Safety
    /// Inputs must originate from the host and remain valid throughout this invocation.
    pub unsafe fn new(
        context: Context,
        api: *const ValuesV1,
        args: *const Value,
        len: usize,
    ) -> Self {
        Self {
            context,
            api: unsafe { &*api },
            args: if len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(args, len) }
            },
        }
    }
    pub fn argument(&self, index: usize) -> Result<Value, String> {
        self.args
            .get(index)
            .copied()
            .ok_or_else(|| format!("Missing argument {index}"))
    }
    fn value(&self, f: impl FnOnce(*mut Value) -> i32) -> Result<Value, String> {
        let mut out = 0;
        if f(&mut out) == 0 {
            Ok(out)
        } else {
            Err("Host value operation failed".into())
        }
    }
    pub fn number(&self, value: Value) -> Result<f64, String> {
        let mut out = 0.0;
        if unsafe { (self.api.number_get)(self.context, value, &mut out) } == 0 {
            Ok(out)
        } else {
            Err("Expected a number".into())
        }
    }
    pub fn string(&self, value: Value) -> Result<String, String> {
        let mut out = Text::new("");
        if unsafe { (self.api.string_get)(self.context, value, &mut out) } != 0 {
            return Err("Expected a string".into());
        }
        let bytes = if out.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(out.ptr, out.len) }
        };
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
    }
    pub fn number_new(&self, number: f64) -> Result<Value, String> {
        self.value(|out| unsafe { (self.api.number_new)(self.context, number, out) })
    }
    pub fn string_new(&self, text: &str) -> Result<Value, String> {
        self.value(|out| unsafe { (self.api.string_new)(self.context, Text::new(text), out) })
    }
    pub fn struct_new(&self) -> Result<Value, String> {
        self.value(|out| unsafe { (self.api.struct_new)(self.context, out) })
    }
    pub fn struct_set(&self, object: Value, key: &str, value: Value) -> Result<(), String> {
        if unsafe { (self.api.struct_set)(self.context, object, Text::new(key), value) } == 0 {
            Ok(())
        } else {
            Err("Host struct operation failed".into())
        }
    }
    /// # Safety
    /// Payload must match the type identifier and drop callback, which must not unwind.
    pub unsafe fn object_new(
        &self,
        kind: &str,
        data: *mut c_void,
        drop: DropObject,
        methods: &str,
    ) -> Result<Value, String> {
        self.value(|out| unsafe {
            (self.api.object_new)(
                self.context,
                Text::new(kind),
                data,
                drop,
                Text::new(methods),
                out,
            )
        })
    }
    /// # Safety
    /// Cast/dereference the returned pointer only as the payload type registered for `kind`.
    pub unsafe fn object_data(&self, value: Value, kind: &str) -> Result<*mut c_void, String> {
        let mut out = std::ptr::null_mut();
        if unsafe { (self.api.object_data)(self.context, value, Text::new(kind), &mut out) } == 0 {
            Ok(out)
        } else {
            Err(format!("Expected {kind}"))
        }
    }
}

/// Catch Rust panics inside the plugin, never at the foreign ABI boundary.
/// The plugin must build with panic=unwind for recovery; panic=abort terminates the app.
/// # Safety
/// `context`, `api`, and `out` must be the live host invocation pointers.
pub unsafe fn guarded(
    context: Context,
    api: *const ValuesV1,
    out: *mut Value,
    body: impl FnOnce() -> Result<Value, String>,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err("Native plugin panicked".into()));
    match result {
        Ok(value) => {
            unsafe { *out = value };
            0
        }
        Err(error) => {
            unsafe { ((*api).error)(context, Text::new(&error)) };
            1
        }
    }
}

/// # Safety
/// Must be called with the pointers supplied to matchbox_plugin_init_v1.
pub unsafe fn require_values(
    host: *const HostV1,
    context: Context,
) -> Result<*const ValuesV1, i32> {
    if host.is_null() {
        return Err(1);
    }
    let host = unsafe { &*host };
    if host.abi_major != ABI_MAJOR {
        return Err(1);
    }
    let api = unsafe { (host.capability)(Text::new(VALUES_V1)) };
    if api.is_null() {
        unsafe {
            (host.error)(
                context,
                Text::new("Missing required capability: matchbox.values.v1"),
            )
        };
        Err(1)
    } else {
        Ok(api.cast())
    }
}
