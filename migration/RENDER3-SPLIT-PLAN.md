# render3 Modular Split — Plan

Goal: split the single `libs/render3` crate into **composable modules** — `core`,
`template`, `decorators` — that **join** via a compiler-plugin registry, mirroring
the `AuthoringPlugin` pattern in `apps/rust/authoring`. Outcome: **fast** (modules
build/test in parallel; a clean per-decorator plugin registry; rayon within) and
**accurate** (focused, independently-tested units). See memory `render3-modular-split`.

## Target structure

```
libs/render3/
  core/        # IR + emit + shared primitives — no template/decorator knowledge
    output_ast, output/{emitter,source_map}, identifiers, factory,
    expression/{lexer,ast,parser}, expression_converter
  template/    # HTML/template -> instruction IR   (depends: core)
    ml_parser, template/{r3_ast,template_transform,control_flow,deferred},
    binder (t2_binder: selectorless + auto-import), view/{template,queries}, i18n
  decorators/  # decorator metadata -> definition  (depends: core, template)
    metadata extraction (today in source_compile),
    view/compiler (compileComponentFromMetadata / compile_directive_from_metadata),
    pipe_module_injector
    + a compiler-plugin registry: ComponentCompiler / DirectiveCompiler /
      PipeCompiler / NgModuleCompiler / InjectableCompiler (extensible, like
      AuthoringPlugin)
  (facade)     # the "join": source_compile (per-file class scan -> dispatch each
               # class to its decorator-compiler plugin -> concatenate) + compile
```

Dependency DAG (no cycles): `core <- template <- decorators <- facade`.

## Compiler-plugin registry (the "join")

Mirror `apps/rust/authoring`'s `AuthoringPlugin`/`AuthoringRegistry`:

```rust
trait DecoratorCompiler {
    fn kind(&self) -> AngularDecoratorKind;            // Component/Directive/Pipe/NgModule/Injectable
    fn compile(&self, class: &ClassMeta, ctx: &CompileCtx) -> CompiledDef;  // -> ɵɵdefine*
}
struct DecoratorRegistry { plugins: Vec<Box<dyn DecoratorCompiler>> }
```

The per-file driver (already iterates every decorated class for multi-class support)
becomes: scan classes -> registry.for_kind(decorator) -> compile -> concatenate.
This is exactly the multi-class loop landed in the compile-any work, refactored
behind the registry so adding a decorator kind is a registration, not an edit.

## Migration approach (keep GREEN at every step)

Execute ONLY when render3 is free (never concurrent with a render3-editing workflow).
Do it in dependency order; re-export from the OLD module paths during the move so
existing call sites keep compiling, then migrate call sites module-by-module:

1. **core** — move the IR/emit/primitives under `core/` (or a `render3_core` crate
   with its own `[lib]`); `pub use` them from `lib.rs` so nothing breaks. Run
   `cargo test -p render3` green.
2. **template** — move the template/binder/view-template/i18n modules under
   `template/`, depending on core. Re-export; green.
3. **decorators** — move the metadata extraction + `view/compiler` +
   `pipe_module_injector`; introduce the `DecoratorCompiler` trait + registry and
   route `source_compile`'s per-class loop through it. Re-export; green.
4. **facade** — `source_compile` + `compile` become the thin join over the registry.
5. Optionally promote `core`/`template`/`decorators` to real workspace sub-crates
   (each its own `Cargo.toml` + `[lib]`) once the module boundaries are stable, so
   they build/test in parallel.

## Hard gates per step

`cargo test -p render3` GREEN; oracle (`parity.mjs`) 27 PASS non-i18n; matchGolden
(via `--cargo-dump`) not regressed (currently 124/182, 580/642 compile-without-error);
0 marker words; the emitted code string byte-identical (the split is a structural
move, not a behavior change).

## Sequencing

Land the in-flight compile-any + i18n accuracy work FIRST (so the split reorganizes
a more-complete render3), then execute this split as a dedicated refactor, module by
module, committing each green.
