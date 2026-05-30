# render3 Port — Architecture Decisions

New crate **`libs/render3`** holds the real direct-to-Ivy Angular compiler, ported from Angular
22.1's `packages/compiler` (`tools/angular-ref`). Specs: `migration/render3-specs/01..16`.

## Key decisions
1. **Owned, arena-free IR for the Output AST** (`output_ast`): use `Box`/`Vec`, NOT oxc's arena
   `'a`. Rationale: render3 builders mutate/clone/dedup expressions freely (constant pool hoisting,
   `isEquivalent`, `clone`). Recommended by spec 01. OXC's bump arena is reached only at the final
   **lowering step** (`output_ast → oxc_ast` via `AstBuilder`, then `oxc_codegen`) — spec 02.
2. **Do NOT reimplement the text emitter.** Lower the output AST to `oxc_ast` and print with
   `oxc_codegen` (precedence-based parens, quoting, source maps). Spec 02.
3. **Transforms use `oxc_traverse` or owned IR, never `Semantic` + `&mut Program` together.**
   Finding from Phase 1a: the legacy `apps/rust/authoring` DI transform held `Semantic<'a>`
   (immutable borrow of `Program`) in an `Rc<RefCell>` while also taking `&mut Program` — an
   inherent aliasing conflict. That is why it was never run. The port avoids in-place AST mutation
   under a live `Semantic`.

## Module port order (dependency-driven)
- **Layer 0 (foundation, no/low deps):** `output_ast` (keystone), `expression::lexer`,
  `expression::ast`, `identifiers` (needs `output_ast::ExternalReference`).
- **Layer 1:** `expression::parser` (lexer+ast), `output::emitter` (output_ast→oxc), `r3_ast`
  (template IR; uses expression::ast).
- **Layer 2:** `template_transform` (HTML→r3_ast), `control_flow`, `deferred`.
- **Layer 3:** `binder` (t2_binder — selectorless/auto-import foundation), `factory`.
- **Layer 4:** `view::compiler` instruction emitter (compileComponentFromMetadata), `view::template`,
  `queries`, `pipe/module/injector`.

Each layer must `cargo check` green before the next builds on it.
