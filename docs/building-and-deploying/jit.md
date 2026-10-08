# JIT Compilation

MatchBox includes a multi-tier JIT compiler built on [Cranelift](https://cranelift.dev/). It profiles hot code at runtime and compiles it to native machine code, eliminating interpreter dispatch overhead for the loops and functions that matter most.

---

## Where JIT is Available

| Build target | JIT active? | Reason |
| :--- | :---: | :--- |
| Native CLI (`matchbox script.bxs`) | Yes | Enabled by default in the native CLI binary |
| Desktop native runner stubs | Opt-in | The CLI forwards its `jit` feature; runners initialize Cranelift before plain or module-envelope execution |
| WASM (browser / container) | No | Cranelift does not target WASM; the browser's own JIT runs the WASM module |
| ESP32 / embedded | No | The `matchbox-esp32-runner` crate is workspace-excluded and has no Cranelift dependency |

The JIT is compiled in via the `jit` Cargo feature, which is included in the default feature set of the `matchbox` CLI crate. The `matchbox_vm` library and desktop runner keep `jit` opt-in (default off). JIT-enabled CLI builds forward the feature only to desktop runner stubs, never WASM/WASI/ESP32. Plain bytecode and module envelopes initialize the same JIT policy. Cached desktop stubs record their feature set so switching build flags cannot silently reuse a non-JIT stub.

For a no-JNI JIT-enabled author build:

```bash
cargo build --no-default-features --features 'bif-io,bif-cli,bif-crypto,bif-http,jit' --target-dir target/lint-jit
python3 tests/jit_runner.py target/lint-jit/debug/matchbox
```

The process check removes source/module copies, verifies numeric results, and requires actual Cranelift compilation of compiler-generated `add`, `max_val`, default/named-argument and guarded leaf functions, plus an empty-loop control, in source, plain/module `.bxb`, and standalone paths. Set `MATCHBOX_JIT_TRACE=1` to log initialization and hot functions rejected by the translatability check; compiled-function messages include names. Trace output goes to stderr. Enabling the feature alone is not evidence that application functions compiled.

---

## The Four Tiers

The JIT uses a tiered strategy: cheap profiling runs first, and compilation only fires once a site has proven itself hot.

### Tier 1 — Empty loop elimination

**Trigger:** A `for` loop whose body is empty, after ≥ 10 000 iterations.

The entire loop is replaced by a single `select` instruction: the counter jumps straight to its final value in one native call. No iteration actually occurs.

### Tier 2 — Numeric loop body

**Trigger:** A `for` loop with a numeric-only body, after ≥ 10 000 cumulative iterations (counter persists across fiber scheduler quanta).

The loop body is translated to Cranelift SSA IR. Local variables live in registers throughout; only the final values are written back to the VM's locals array on exit. Inline-cached struct member reads (monomorphic or bi-morphic) are also supported.

### Tier 3 — Array iteration

**Trigger:** A `for-in` loop over a float-only array, after ≥ 5 000 cumulative iterations.

The array data pointer and length are passed directly to the compiled loop, bypassing the iterator protocol entirely.

### Tier 4 — Hot function compilation

**Trigger:** A function body that passes a translatability check, after ≥ 100 calls.

Compilation starts at the body immediately after `FUNCTION_ENTER`, not at the function's prologue. Ordinary VM frames still bind positional/named/default arguments, initialize scopes and run entry interception; compiled results use ordinary return/exit interception. All call paths that execute this marker are covered. Debugger stepping stays interpreted.

The initial safe subset is readonly, nonallocating, forward-only leaves:

| Category | Opcodes |
| :--- | :--- |
| Parameters | `GET_LOCAL` within declared arity, including the arguments-scope slot |
| Constants | Number, Boolean, null |
| Arithmetic | `ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE` and float variants |
| Comparison | `==`, `!=`, `<`, `<=`, `>`, `>=` |
| Boolean | `NOT`, Boolean-guarded conditions |
| Control flow | `JUMP`, `JUMP_IF_FALSE` (forward jumps) |
| Stack | `POP`, `DUP` |
| Return | `RETURN` |

Arithmetic/comparison guards require non-NaN floats. Integers, coercions, non-Boolean truthiness, zero divisors and NaN-producing arithmetic safely **deopt to body entry**, without replaying defaults/interception or losing scope mutations. Writes, calls and allocation remain interpreted; the old direct compiled-callee path and relaxed coercion helpers were removed because they bypassed these semantics. Program ownership guards prevent stale function/address reuse across GC; JIT state remains VM-local.

---

## What Is Not Supported in Tier 4

The following categories cause a function to remain interpreted:

- **Global variables** — `GET_GLOBAL`, `SET_GLOBAL`, `DEFINE_GLOBAL`, `GET_PRIVATE`, `SET_PRIVATE`
- **I/O** — `PRINT`, `PRINTLN`
- **Object / collection creation** — `NEW`, `ARRAY`, `STRUCT`
- **Property and index access** — `MEMBER`, `SET_MEMBER`, `INC_MEMBER`, `INDEX`, `SET_INDEX`
- **Calls inside compiled bodies** — `CALL`, `INVOKE`, `INVOKE_NAMED`, `CALL_NAMED`
- **Local mutation/declarations** — `SET_LOCAL`, `SET_LOCAL_POP`, `BIND_LOCAL`
- **Strings/remainder/integer specializations** — `STRING_CONCAT`, `MODULO`, `ADD_INT`, `SUB_INT`, `MUL_INT`
- **Loops inside functions** — `FOR_LOOP_STEP`, `LOOP`, `ITER_NEXT`
- **Exception handling** — `PUSH_HANDLER`, `POP_HANDLER`, `THROW`
- **Multi-word compare-jumps** — `LOCAL_COMPARE_JUMP`, `GLOBAL_COMPARE_JUMP`, `COMPARE_JUMP`, `LOCAL_JUMP_IF_NE_CONST`
- **Miscellaneous** — `INC`, `DEC`, `INC_LOCAL`, `INC_GLOBAL`, `SWAP`, `OVER`, `JUMP_IF_NULL`

This bounded subset enables actual numeric parameter-only helpers, not general functions, recursion or analyzer acceleration by itself.

---

## Opting Out

If you need to build the CLI without JIT (e.g. to reduce binary size for a constrained native target):

```bash
cargo build --release --no-default-features \
  --features "bif-io,bif-jni,bif-crypto,bif-cli"
```

---

## Future Directions

### Measure eligibility, not just activation

A historical bounded bx-lint LSP experiment, before body-entry compilation, initialized Cranelift and reached the hot-function threshold in 35 helpers, but compiled none: all observed candidates were rejected for unsupported bytecode. Opening a 200-function script remained about 1.7 seconds, with or without JIT. Even the current pure-math `jit_hot_fn.bxs` functions were rejected in that build; existing output-only tests do not prove Tier-4 activation. The current runner check above additionally proves real compiler-function compilation after the body-entry fix. These are build/workload observations, not a general JIT speed guarantee.

The tested Linux bx-lint standalone grew from 5,821,860 to 7,607,060 bytes; actual linked size depends on target, features and optimization. Do not assume a fixed Cranelift size penalty.

### Wider Tier-4 opcode coverage

The most impactful missing opcodes for real-world functions are globals and property access. Runtime helpers such as `jit_ic_member_fallback` illustrate guarded reads. The main constraint is the deopt story: any operation that can throw or have side effects needs a clean deopt path back to the interpreter.

### String constant support in Tier-4

General string values remain unsupported. Adding them requires GC-rooted VM values and safepoints; embedding unguarded GC IDs in machine code is not safe.

### Tier-4 for embedded loops

Functions containing `FOR_LOOP_STEP` are currently rejected by `fn_is_translatable`. Supporting them would require the Tier-2 loop compiler to operate within Tier-4's SSA frame, essentially merging the two tiers. The deopt model is the main complexity: a loop inside a compiled function that deopts mid-iteration needs to restore both the function's locals and the loop counter.
