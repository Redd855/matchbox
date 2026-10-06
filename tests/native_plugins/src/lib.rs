use matchbox_plugin_sdk::*;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicUsize, Ordering},
};
static DROPS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn drop_object(data: *mut c_void) {
    drop(unsafe { Box::from_raw(data.cast::<f64>()) });
    DROPS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn make(
    ctx: Context,
    api: *const ValuesV1,
    _: *mut c_void,
    args: *const Value,
    len: usize,
    out: *mut Value,
) -> i32 {
    unsafe {
        guarded(ctx, api, out, || {
            let call = Call::new(ctx, api, args, len);
            let value = call.number(call.argument(0)?)?;
            let data = Box::into_raw(Box::new(value)).cast();
            match call.object_new(
                "fixture.number.v1",
                data,
                drop_object,
                r#"{"info":"fixtureInfo"}"#,
            ) {
                Ok(value) => Ok(value),
                Err(error) => {
                    drop_object(data);
                    Err(error)
                }
            }
        })
    }
}
unsafe extern "C" fn info(
    ctx: Context,
    api: *const ValuesV1,
    _: *mut c_void,
    args: *const Value,
    len: usize,
    out: *mut Value,
) -> i32 {
    unsafe {
        guarded(ctx, api, out, || {
            let call = Call::new(ctx, api, args, len);
            let number = *call
                .object_data(call.argument(0)?, "fixture.number.v1")?
                .cast::<f64>();
            let result = call.struct_new()?;
            call.struct_set(result, "value", call.number_new(number)?)?;
            Ok(result)
        })
    }
}
unsafe extern "C" fn drops(
    ctx: Context,
    api: *const ValuesV1,
    _: *mut c_void,
    args: *const Value,
    len: usize,
    out: *mut Value,
) -> i32 {
    unsafe {
        guarded(ctx, api, out, || {
            Call::new(ctx, api, args, len).number_new(DROPS.load(Ordering::SeqCst) as f64)
        })
    }
}
unsafe extern "C" fn panic_callback(
    ctx: Context,
    api: *const ValuesV1,
    _: *mut c_void,
    _: *const Value,
    _: usize,
    out: *mut Value,
) -> i32 {
    unsafe { guarded(ctx, api, out, || panic!("fixture panic")) }
}
unsafe extern "C" fn bad_handle(
    ctx: Context,
    api: *const ValuesV1,
    _: *mut c_void,
    _: *const Value,
    _: usize,
    out: *mut Value,
) -> i32 {
    unsafe {
        guarded(ctx, api, out, || {
            Call::new(ctx, api, std::ptr::null(), 0)
                .number(9999)
                .and_then(|n| Call::new(ctx, api, std::ptr::null(), 0).number_new(n))
        })
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn matchbox_plugin_init_v1(host: *const HostV1, ctx: Context) -> i32 {
    if unsafe { require_values(host, ctx) }.is_err() {
        return 1;
    }
    let host = unsafe { &*host };
    if std::env::var_os("MATCHBOX_FIXTURE_MISSING_CAPABILITY").is_some() {
        if unsafe { (host.capability)(Text::new("missing.capability.v1")) }.is_null() {
            unsafe {
                (host.error)(
                    ctx,
                    Text::new("Missing required capability: missing.capability.v1"),
                )
            };
            return 1;
        }
    }
    let functions = [
        (
            r#"{"name":"fixtureNew","aliases":["fixtureCreate"],"arguments":[{"name":"value","default":42}]}"#,
            make as Invoke,
        ),
        (
            r#"{"name":"fixtureInfo","arguments":[{"name":"object","required":true}]}"#,
            info as Invoke,
        ),
        (r#"{"name":"fixtureDrops"}"#, drops as Invoke),
        (r#"{"name":"fixturePanic"}"#, panic_callback as Invoke),
        (r#"{"name":"fixtureBadHandle"}"#, bad_handle as Invoke),
    ];
    for (metadata, invoke) in functions {
        if unsafe { (host.register)(ctx, Text::new(metadata), invoke) } != 0 {
            return 1;
        }
    }
    0
}
