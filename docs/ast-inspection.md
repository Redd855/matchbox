# AST inspection

`matchbox-parser` owns the existing lexer, script/template compiler AST, parser and CST. Both `matchbox-compiler` and `matchbox-vm` depend on it. There is one frontend, no compiler/VM dependency cycle, and parsing does not execute inspected code.

## BoxLang contract

```boxlang
ast = boxAST(source="function add(a,b) { return a+b; }");
ast = boxAST(filepath="src/Example.bx");
json = "x=1+2".toAST(returnType="json");
editor = boxAST(source="var good=1; var broken=;", recover=true, includeTokens=true);
```

Arguments retain the JVM order: `source`, `filepath`, `returnType="struct"`, `sourceType="script"`. MatchBox adds Boolean `recover=false` and `includeTokens=false`. Nonblank source takes precedence over filepath. File input requires `bif-io`; `.bxm` is recognized as template input. Strict blank/no input fails; editor recovery accepts an empty document.

- `struct`: a real nested AST rooted at `BoxScript`.
- `json`: the same exported data as JSON.
- `text`: original parsed source text; not a formatting/PrettyPrint implementation.
- Strict parsing throws parse/lexical errors. `recover=true` preserves completed top-level statements and adds `issues` containing message, filename, position and span. It does not fabricate a successful complete tree for malformed input.
- `includeTokens=true` adds source-backed `tokens` and `trivia` (whitespace and comments). Comments are also collected on the root; JVM comment-to-node attachment is not reproduced.

Nodes use `ASTType`, `ASTPackage`, `sourceText`, `comments` and node-specific fields. The exporter is separate from internal Rust enum layouts. Familiar JVM node names/fields are used where retained by the compiler; compiler-only forms use explicit `MatchBox*` names. This is a **bounded exported schema, not exact JVM AST parity**.

## Positions

- `position.start/end`: **one-based lines, zero-based UTF-16 columns**, end exclusive. LSP adapters subtract one from lines.
- `span.start/end`: **zero-based UTF-8 byte offsets**, end exclusive, into the supplied/read source.
- Source-backed script statements/expressions, tokens and trivia have spans. Compiler-synthesized/desugared subnodes without retained spans omit position/span and have null sourceText rather than guessed coordinates. Interpolation expression offsets are translated back into the containing source.
- Internal lexer `Span.col` remains one-based Unicode-scalar indexing. Consumers must not confuse it with the exported UTF-16 column.

## Current boundaries

The compiler AST loses some original syntax: compound assignments and some unary forms are desugared; assignment targets, parameters, properties, argument names, annotations and other synthesized nodes do not yet all retain individual spans. Recovery currently synchronizes at top-level statement boundaries and discards incomplete nested declarations. Full source-preserving node/schema parity requires further frontend work.

`sourceType="template"`, `"cfscript"` and `"cftemplate"` currently fail explicitly. In particular, the template compiler currently fabricates loop/condition/function data; exposing those results as faithful analysis would be incorrect. Existing template compilation remains available and is not certified by the inspection API.

Checks: `cargo test -p matchbox_parser` and `cargo test --test integration_tests vm_box_ast`. The public non-extension BoxLang contract in `tests/scripts/vm_box_ast.bxs` is also exercised against the JVM reference.
