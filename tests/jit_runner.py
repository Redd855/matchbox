#!/usr/bin/env python3
"""A JIT-enabled CLI must activate Cranelift in source, bytecode and standalone paths."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
cli = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix="matchbox-jit-runner-") as directory:
    work = Path(directory)
    source = work / "probe.bxs"
    source.write_text((root / "tests/scripts/jit_hot_fn.bxs").read_text() + "\n" +
                      (root / "tests/scripts/jit_function_contract.bxs").read_text() +
                      "\nfor (var warm=0; warm<20000; warm++) {}\n")
    module = work / "module"
    module.mkdir()
    (module / "box.json").write_text('{"slug":"jit-probe","boxlang":{"moduleName":"jit-probe"}}')
    (module / "ModuleConfig.bx").write_text("class {}")
    env = dict(os.environ, BOXLANG_HOME=str(work / "home"), MATCHBOX_JIT_TRACE="1")

    def check(command):
        result = subprocess.run(command, cwd=work, env=dict(env, PATH="/usr/bin:/bin"), capture_output=True, timeout=60)
        assert result.returncode == 0, (command, result.stdout, result.stderr)
        assert result.stdout.splitlines() == [b"19900", b"199", b"Function JIT / defaults / named calls / guards / scopes / GC: OK"], result.stdout
        assert b"[JIT] compiled empty loop" in result.stderr, (command, "Cranelift never activated", result.stderr)
        for name in ["add", "max_val", "weighted", "choose", "divide", "ready"]:
            assert f"[JIT] compiled hot fn name={name} ".encode() in result.stderr, (command, "Real compiler function never compiled", result.stderr)
        assert b"[JIT] deopt hot fn body (type guard)" in result.stderr, result.stderr

    check([str(cli), str(source)])
    outputs = []
    for bundled in [False, True]:
        modules = ["--module", str(module)] if bundled else []
        for target, flags in [("bxb", ["--build"]), ("native", ["--target", "native"])]:
            output = work / f"{'module' if bundled else 'plain'}.{target}"
            result = subprocess.run([str(cli), "--no-shaking", *modules, *flags, str(source), "--output", str(output)], cwd=work, env=env, capture_output=True, timeout=60)
            assert result.returncode == 0, result.stderr
            outputs.append((target, output))
    source.unlink()
    for file in module.iterdir():
        file.unlink()
    module.rmdir()
    for index, (target, output) in enumerate(outputs):
        env["MATCHBOX_NATIVE_CACHE"] = str(work / f"cache-{index}")
        check([str(cli), str(output)] if target == "bxb" else [str(output)])
print("Source-free JIT activation / numeric correctness across CLI and runner paths: OK")
