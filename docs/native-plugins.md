# Prebuilt native modules (desktop)

A module remains one BoxLang package: `box.json`, `ModuleConfig.bx`, shared `bifs/` and models, JVM JARs in `libs/`, and optional prebuilt MatchBox artifacts. JVM and MatchBox backends share the module identity and public BoxLang API, not Java/Rust object layouts.

## Artifact metadata

Add an optional `matchbox.artifacts` map to `box.json`, keyed by the output target triple:

```json
{
  "slug": "bx-image",
  "type": "boxlang-modules",
  "boxlang": { "moduleName": "image" },
  "matchbox": {
    "artifacts": {
      "x86_64-unknown-linux-gnu": {
        "path": "any/package/path/libimage.so",
        "sha256": "<64 lowercase hexadecimal characters>",
        "abi": 1
      }
    }
  }
}
```

Specify exactly one `path` (inside the installed package) or `url` (HTTPS). URLs are resolved by the compiler/CLI, require its `bif-http` feature, and are limited to 256 MiB. Verify the hash before execution. Standalone executables and module-bearing `.bxb` artifacts contain the selected native bytes; they never download libraries at startup. Native source layout and artifact filenames are not prescribed. No target match is an error, not a request to compile the module locally.

Publish separate artifacts for OS, architecture, and libc. Authors must choose appropriate CPU and OS/libc baselines: a target triple alone cannot guarantee those. Dependent system libraries must already be available; bundling dependent shared libraries is not implemented yet.

## Runtime startup

Discovery reads metadata and compiles descriptors without executing lifecycle hooks. Module names honor `boxlang.moduleName`; literal descriptor mappings and dependency arrays are supported. Dependencies initialize first; missing dependencies and cycles fail. Disabled modules are omitted. Application overrides come from `boxlang.json` → `modules.<name>.settings`.

At execution, load native providers, instantiate descriptors, call `configure()`, read the descriptor's **variables-scope `settings`**, deep-merge application settings, and register BoxLang class BIFs using their `invoke()` method. After initialization, call `onLoad()` in dependency order. Call `onUnload()` in reverse order at normal shutdown, including application errors. This does not provide cleanup after process abort/crash.

`configure()` populates `settings`; its return value is ignored. Old build-time lifecycle execution and baked settings are removed. Class BIF names currently come from filenames; class annotation aliases/member declarations, tags, interceptors, dynamic/struct-valued mappings, and moduleRecord/service injection are not yet implemented. Native objects can declare member-to-BIF mappings. A module relying on unsupported JVM APIs must provide an alternative backend.

## Native SDK / ABI

Build a Rust `cdylib` using `crates/matchbox-plugin-sdk`. Export `matchbox_plugin_init_v1`. The host supplies a C-layout registration interface and named capability lookup. The first capability is `matchbox.values.v1`: numbers, strings, booleans/null, structs, opaque plugin objects, and error reporting. BIF metadata declares names, aliases, argument names, required arguments, and scalar defaults. Named arguments are case-insensitive; unknown/duplicate arguments fail.

The SDK depends only on Rust's standard library and does not link MatchBox's VM. No `String`, `Vec`, `Rc`, `BxVM`, NaN-box layout, or Rust trait-object vtable crosses the ABI. SDK helpers wrap the C interface for Rust authors.

Compatibility rules:

- ABI major and each named capability's layout/signature/semantics are immutable.
- Add new operations through new capabilities; retain old capabilities for existing binaries.
- Unknown/missing required capabilities cause explicit initialization failures.
- Value handles and borrowed strings/context pointers last **one invocation**. The optional `matchbox.roots.v1` capability provides separate persistent roots; it does not extend the lifetime of invocation handles.
- Plugin payloads are opaque, plugin-owned pointers, scoped to the provider instance and type name. Ownership transfers to the host only when object creation succeeds. The host invokes the plugin's destructor when the object is collected.
- Host and plugin each free their own allocations. Initialized libraries remain loaded for the process lifetime, including when plugin state/destructors outlive a VM.
- All operations are synchronous on the VM thread. No async, unload, or hot reload.
- Catch panics inside Rust callbacks using `guarded`; never unwind across C ABI callbacks. Recovery needs `panic = "unwind"`; aborting panics terminate the process.

Plugins are trusted arbitrary native code, not sandboxed. SHA-256 pins content; it does not establish publisher trust. Cache permissions prevent other users from substituting code; same-user compromise is outside this trust model.

## Retained VM values

`matchbox.roots.v1` is an additive capability; `HostV1`, `ValuesV1`, and the ABI major are unchanged, so existing prebuilt plugins need no rebuild. Plugins requiring roots request `require_roots(host, context)` during initialization; older hosts report the missing capability explicitly.

The capability has three operations: retain an invocation value as an opaque host-owned root, resolve a root into a new invocation value, and release the root. The SDK wraps them with RAII:

```rust
// roots: &'static RootsV1, obtained during plugin initialization
let saved = call.retain(roots, call.argument(0)?)?;
// Move saved into plugin-owned state. During a later invocation:
let value = saved.value(&later_call)?;
// Explicit release, or let the owning Rust payload's destructor do it:
drop(saved);
```

Roots keep the value and its ordinary GC-reachable children alive through minor and major collection, including a callback's captured receiver. Root reads validate both VM and provider identity. A root does **not** keep its VM alive: reads using another VM fail, but release needs no invocation context and remains safe after VM shutdown.

All root operations, including destruction, must remain on the VM thread. The Rust wrapper is neither `Send` nor `Sync` and cannot be cloned; retaining the same value again creates an independent root. Raw C root pointers must be live, host-issued allocations and released exactly once. Strong roots cannot break cycles: do not retain a value that refers back to its own native holder unless the plugin provides an explicit release path.

## Packaging / cache

```bash
matchbox app.bxs --module /path/to/package
matchbox --build app.bxs --module /path/to/package --output app.bxb
matchbox --target native app.bxs --module /path/to/package --output app
```

Native libraries are extracted into a private content-addressed cache by target/hash. Publication is atomic across concurrent launches; cached bytes are verified before loading. Tampered entries fail rather than being silently trusted. Set `MATCHBOX_NATIVE_CACHE` to choose the directory. Otherwise use the platform's user cache directory (or a process-specific temporary directory when no home/cache directory is available).

Source-based Native Fusion remains available with `--target native`. A source-only Rust module cannot be executed directly by the CLI; publish a prebuilt artifact or use Native Fusion. Native module envelopes currently target desktop execution, not ESP32 or WebAssembly.

## Checks

```bash
cargo test --test native_plugin_tests
cargo test -p matchbox_vm
cargo test -p matchbox-plugin-sdk
```

The native fixture tests runtime lifecycle, shared class BIFs, named/default arguments, object collection, structured result lifetime, retained values/callbacks, VM/provider isolation, root release during GC and after VM shutdown, caught plugin panics, missing capabilities, envelope packaging, cache reuse, concurrent extraction, and artifact rejection. The separate `bx-image` pilot runs an identical BoxLang contract against its existing JVM JAR and native provider.
