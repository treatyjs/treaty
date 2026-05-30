// @ts-check
/**
 * Oracle parity-diff harness for the Treaty `render3` Rust compiler.
 *
 * For each fixture template it:
 *   1. Compiles via @angular/compiler (parseTemplate + compileComponentFromMetadata)
 *      to obtain the *oracle* Ivy `ɵɵdefineComponent({...})` JS. This mirrors what
 *      apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts does, using the same custom
 *      output printer (ported inline below from .../treaty-sfc/printer.ts, since
 *      @angular/compiler does not export a public JS emitter).
 *   2. Compiles via the Rust NAPI addon `@treaty/authoring-node`
 *      (`compile_component(template, selector, className) -> { code, errors }`).
 *   3. NORMALIZES both outputs (strip whitespace, unify the i0/import prefix,
 *      loosely sort instruction call arguments) and prints a per-fixture
 *      PASS / DIFF report.
 *
 * This file is INDEPENDENT of the Rust crate and never edits libs/render3 source.
 *
 * Build the addon first (see README.md):
 *   cd libs/authoring/node && npx napi build --platform
 * then run:
 *   node libs/render3/parity/parity.mjs
 *
 * If the addon cannot be built/loaded on this machine the harness still runs the
 * oracle side and reports the blocker per fixture instead of crashing.
 */

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import * as ng from '@angular/compiler';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..');
const require = createRequire(import.meta.url);

// ---------------------------------------------------------------------------
// Fixtures: (id, template, selector, className). Selector/className are fed
// identically to both compilers so only template lowering can differ.
// ---------------------------------------------------------------------------
const FIXTURES = [
  {
    id: 'static-element',
    template: '<button>Hi</button>',
    selector: 'app-btn',
    className: 'BtnComponent',
  },
  {
    id: 'interpolation',
    template: '<div>{{name}}</div>',
    selector: 'app-hello',
    className: 'HelloComponent',
  },
  {
    id: 'nested',
    template: '<div><span>{{name}}</span></div>',
    selector: 'app-nested',
    className: 'NestedComponent',
  },
  {
    id: 'attribute',
    template: '<div class="box" id="x">y</div>',
    selector: 'app-attr',
    className: 'AttrComponent',
  },
  {
    id: 'multiple-bindings',
    template: '<p>{{a}} and {{b}}</p>',
    selector: 'app-multi',
    className: 'MultiComponent',
  },
  // -------------------------------------------------------------------------
  // Wider corpus: the next batch of template shapes that surface Rust-vs-Angular
  // divergence. NOTE: the Rust `compile_component` always feeds @angular/compiler
  // EMPTY inputs/outputs (it does not scan the template for referenced
  // bindings), so the oracle here likewise uses `inputs: {}, outputs: {}` to keep
  // the comparison apples-to-apples — a property/event/class/style binding still
  // lowers to its instruction stream without the component declaring an input or
  // output, which is exactly what both sides do.
  // -------------------------------------------------------------------------
  {
    id: 'property-binding',
    template: '<div [id]="x"></div>',
    selector: 'app-prop',
    className: 'PropComponent',
  },
  {
    id: 'event-binding',
    template: '<button (click)="f()">go</button>',
    selector: 'app-event',
    className: 'EventComponent',
  },
  {
    id: 'two-interpolations',
    template: '<p>{{a}} {{b}}</p>',
    selector: 'app-two-interp',
    className: 'TwoInterpComponent',
  },
  {
    id: 'class-binding',
    template: '<div [class.on]="b">y</div>',
    selector: 'app-class',
    className: 'ClassComponent',
  },
  {
    id: 'style-binding',
    template: '<div [style.color]="c">y</div>',
    selector: 'app-style',
    className: 'StyleComponent',
  },
  {
    id: 'control-flow-if',
    template: '<div>@if (cond) { <span>a</span> }</div>',
    selector: 'app-if',
    className: 'IfComponent',
  },
  {
    id: 'control-flow-for',
    template: '<ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul>',
    selector: 'app-for',
    className: 'ForComponent',
  },
  {
    id: 'deep-nesting',
    template: '<div><p><span>{{x}}</span></p></div>',
    selector: 'app-deep',
    className: 'DeepComponent',
  },
  {
    id: 'sibling-elements',
    template: '<div></div><span></span>',
    selector: 'app-siblings',
    className: 'SiblingsComponent',
  },
  {
    id: 'static-and-bound-mix',
    template: '<div class="box" [id]="x">{{t}}</div>',
    selector: 'app-mix',
    className: 'MixComponent',
  },
  // -------------------------------------------------------------------------
  // Third batch: control-flow chains/branches and richer binding combos. Same
  // empty-inputs/outputs convention applies — these templates exercise lowering
  // shapes (block chains, $index/$count implicit vars, multi-binding elements,
  // ng-template refs, attr.* bindings) without the component declaring inputs or
  // outputs, so the oracle keeps inputs:{}/outputs:{} like the Rust side.
  // -------------------------------------------------------------------------
  {
    id: 'control-flow-if-elseif-else',
    template:
      '<div>@if (a) { <span>x</span> } @else if (b) { <span>y</span> } @else { <span>z</span> }</div>',
    selector: 'app-if-chain',
    className: 'IfChainComponent',
  },
  {
    id: 'control-flow-switch',
    template:
      '<div>@switch (k) { @case (1) { <span>one</span> } @case (2) { <span>two</span> } @default { <span>other</span> } }</div>',
    selector: 'app-switch',
    className: 'SwitchComponent',
  },
  {
    id: 'nested-for-in-if',
    template: '<div>@if (cond) { <ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul> }</div>',
    selector: 'app-nested-for-if',
    className: 'NestedForIfComponent',
  },
  {
    id: 'multiple-event-bindings',
    template: '<button (click)="f()" (mouseenter)="g()">go</button>',
    selector: 'app-multi-event',
    className: 'MultiEventComponent',
  },
  {
    id: 'mixed-bindings-element',
    template: '<input [value]="v" (input)="o($event)" [class.err]="e">',
    selector: 'app-mixed-bind',
    className: 'MixedBindComponent',
  },
  {
    id: 'for-index-count',
    template:
      '<ul>@for (x of xs; track x) { <li>{{ $index }} of {{ $count }}: {{x}}</li> }</ul>',
    selector: 'app-for-index',
    className: 'ForIndexComponent',
  },
  {
    id: 'ng-template-ref',
    template: '<ng-template #tpl><span>tpl</span></ng-template>',
    selector: 'app-tpl-ref',
    className: 'TplRefComponent',
  },
  {
    id: 'attr-binding',
    template: '<div [attr.role]="r"></div>',
    selector: 'app-attr-bind',
    className: 'AttrBindComponent',
  },
];

// ---------------------------------------------------------------------------
// Output printer — ported from apps/repl/src/tools/treaty-sfc/printer.ts.
// A Huge thanks to Alex Rickabaugh for the original. Implements the subset of
// ExpressionVisitor / StatementVisitor needed to serialise the definition.
// ---------------------------------------------------------------------------
function makePrinter() {
  const UNARY_OPERATORS = new Map([
    [ng.UnaryOperator.Minus, '-'],
    [ng.UnaryOperator.Plus, '+'],
  ]);
  const BINARY_OPERATORS = new Map([
    [ng.BinaryOperator.And, '&&'],
    [ng.BinaryOperator.Bigger, '>'],
    [ng.BinaryOperator.BiggerEquals, '>='],
    [ng.BinaryOperator.BitwiseAnd, '&'],
    [ng.BinaryOperator.BitwiseOr, '|'],
    [ng.BinaryOperator.Divide, '/'],
    [ng.BinaryOperator.Equals, '=='],
    [ng.BinaryOperator.Identical, '==='],
    [ng.BinaryOperator.Lower, '<'],
    [ng.BinaryOperator.LowerEquals, '<='],
    [ng.BinaryOperator.Minus, '-'],
    [ng.BinaryOperator.Modulo, '%'],
    [ng.BinaryOperator.Multiply, '*'],
    [ng.BinaryOperator.NotEquals, '!='],
    [ng.BinaryOperator.NotIdentical, '!=='],
    [ng.BinaryOperator.Or, '||'],
    [ng.BinaryOperator.Plus, '+'],
    [ng.BinaryOperator.NullishCoalesce, '??'],
    // `=` assignment. The original printer.ts port omitted this; @switch lowering
    // emits an assignment expression (the switch-value temp), so without it the
    // oracle THROWS "Unknown binary operator: Assign" and the fixture is scored a
    // DIFF purely because the oracle could not render — not a Rust divergence.
    // Adding the real operator lets the @switch oracle compile and be compared.
    [ng.BinaryOperator.Assign, '='],
  ]);

  class Context {
    constructor(isStatement) { this.isStatement = isStatement; }
    get withExpressionMode() { return this.isStatement ? new Context(false) : this; }
    get withStatementMode() { return !this.isStatement ? new Context(true) : this; }
  }

  class Printer {
    visitDeclareVarStmt(stmt, context) {
      let varStmt = stmt.hasModifier(ng.StmtModifier.Final) ? 'const' : 'let';
      varStmt += ' ' + stmt.name;
      if (stmt.value) varStmt += ' = ' + stmt.value.visitExpression(this, context.withExpressionMode);
      // Terminate with ';' like every other statement visitor (visitExpressionStmt /
      // visitReturnStmt both append ';'). The original printer.ts port omitted it,
      // relying on the '\n' join in visitStatements for separation. But normalize()
      // collapses ALL whitespace, so a missing ';' fuses the var decl with the next
      // instruction (e.g. `const x_r1=ctx.$implicitɵɵadvance()`), which the Rust
      // emitter — correctly — terminates (`const x_r1=ctx.$implicit;ɵɵadvance()`).
      // This is a printer-port omission artifact, not a Rust lowering divergence.
      return varStmt + ';';
    }
    visitDeclareFunctionStmt(stmt, context) {
      let fn = `function ${stmt.name}(${stmt.params.map((p) => p.name).join(', ')}) {`;
      fn += this.visitStatements(stmt.statements, context.withStatementMode);
      fn += '}';
      return fn;
    }
    visitExpressionStmt(stmt, context) {
      return stmt.expr.visitExpression(this, context.withStatementMode) + ';';
    }
    visitReturnStmt(stmt, context) {
      return 'return ' + stmt.value.visitExpression(this, context.withExpressionMode) + ';';
    }
    visitIfStmt(stmt, context) {
      let s = 'if (' + stmt.condition.visitExpression(this, context) + ') {';
      s += this.visitStatements(stmt.trueCase, context.withStatementMode) + '}';
      if (stmt.falseCase.length > 0) {
        s += ' else {' + this.visitStatements(stmt.falseCase, context.withStatementMode) + '}';
      }
      return s;
    }
    visitReadVarExpr(ast) { return ast.name; }
    visitWriteVarExpr(expr, context) {
      const a = `${expr.name} = ${expr.value.visitExpression(this, context)}`;
      return context.isStatement ? a : `(${a})`;
    }
    visitWriteKeyExpr(expr, context) {
      const c = context.withExpressionMode;
      const a = `${expr.receiver.visitExpression(this, c)}[${expr.index.visitExpression(this, c)}] = ${expr.value.visitExpression(this, c)}`;
      return context.isStatement ? a : `(${a})`;
    }
    visitWritePropExpr(expr, context) {
      return `${expr.receiver.visitExpression(this, context)}.${expr.name} = ${expr.value.visitExpression(this, context)}`;
    }
    visitInvokeFunctionExpr(ast, context) {
      const fn = ast.fn.visitExpression(this, context);
      const args = ast.args.map((arg) => arg.visitExpression(this, context));
      return `${fn}(${args.join(', ')})`;
    }
    visitTaggedTemplateExpr() { throw new Error('only important for i18n'); }
    visitInstantiateExpr(ast, context) {
      const ctor = ast.classExpr.visitExpression(this, context);
      const args = ast.args.map((arg) => arg.visitExpression(this, context));
      return `new ${ctor}(${args.join(', ')})`;
    }
    visitLiteralExpr(ast) {
      let value;
      if (typeof ast.value === 'string') value = `'` + ast.value.replaceAll(`'`, `\\'`) + `'`;
      else if (ast.value === undefined) value = 'undefined';
      else if (ast.value === null) value = 'null';
      else value = ast.value.toString();
      return value;
    }
    visitLocalizedString() { throw new Error('only important for i18n'); }
    visitExternalExpr(ast) {
      if (ast.value.name === null) {
        if (ast.value.moduleName === null) throw new Error('Invalid import without name nor moduleName');
        return 'i0';
      }
      return ast.value.moduleName !== null ? `i0.${ast.value.name}` : ast.value.name;
    }
    visitConditionalExpr(ast, context) {
      let cond = ast.condition.visitExpression(this, context);
      if (ast.condition instanceof ng.ConditionalExpr) cond = `(${cond})`;
      return cond + ' ? ' + ast.trueCase.visitExpression(this, context) + ' : ' + ast.falseCase.visitExpression(this, context);
    }
    visitDynamicImportExpr(ast) { return `import('${ast.url}')`; }
    visitNotExpr(ast, context) { return '!' + ast.condition.visitExpression(this, context); }
    visitFunctionExpr(ast, context) {
      let fn = 'function ';
      if (ast.name) fn += ast.name;
      fn += '(' + ast.params.map((p) => p.name).join(', ') + ') {';
      fn += this.visitStatements(ast.statements, context);
      fn += '}';
      return fn;
    }
    visitArrowFunctionExpr(ast, context) {
      const params = ast.params.map((p) => p.name).join(', ');
      const body = Array.isArray(ast.body)
        ? '{' + this.visitStatements(ast.body, context) + '}'
        : ast.body.visitExpression(this, context);
      return `(${params}) => ${body}`;
    }
    visitBinaryOperatorExpr(ast, context) {
      if (!BINARY_OPERATORS.has(ast.operator)) throw new Error(`Unknown binary operator: ${ng.BinaryOperator[ast.operator]}`);
      return ast.lhs.visitExpression(this, context) + BINARY_OPERATORS.get(ast.operator) + ast.rhs.visitExpression(this, context);
    }
    visitReadPropExpr(ast, context) { return ast.receiver.visitExpression(this, context) + '.' + ast.name; }
    visitReadKeyExpr(ast, context) {
      return `${ast.receiver.visitExpression(this, context)}[${ast.index.visitExpression(this, context)}]`;
    }
    visitLiteralArrayExpr(ast, context) {
      return '[' + ast.entries.map((e) => e.visitExpression(this, context)).join(', ') + ']';
    }
    visitLiteralMapExpr(ast, context) {
      const props = ast.entries.map((entry) => {
        let key = entry.key;
        if (entry.quoted) key = `'` + key.replaceAll(`'`, `\\'`) + `'`;
        return key + ': ' + entry.value.visitExpression(this, context);
      });
      return '{' + props.join(', ') + '}';
    }
    visitCommaExpr() { throw new Error('Method not implemented.'); }
    visitWrappedNodeExpr(ast) { return ast.node; }
    visitTypeofExpr(ast, context) { return 'typeof ' + ast.expr.visitExpression(this, context); }
    visitVoidExpr(ast, context) { return 'void ' + ast.expr.visitExpression(this, context); }
    visitUnaryOperatorExpr(ast, context) {
      if (!UNARY_OPERATORS.has(ast.operator)) throw new Error(`Unknown unary operator: ${ng.UnaryOperator[ast.operator]}`);
      return UNARY_OPERATORS.get(ast.operator) + ast.expr.visitExpression(this, context);
    }
    visitStatements(statements, context) {
      return statements
        .map((stmt) => stmt.visitStatement(this, context))
        .filter((s) => s !== undefined)
        .join('\n');
    }
  }

  return { Context, Printer };
}

// ---------------------------------------------------------------------------
// Oracle: compile a fixture with @angular/compiler to the Ivy definition JS.
// Mirrors treat-to-ivy.ts but with no inputs/outputs/queries (matches the
// minimal metadata the Rust compile_component builds).
// ---------------------------------------------------------------------------
function compileWithOracle({ template, selector, className }) {
  const parsed = ng.parseTemplate(template, `${className}.html`);
  if (parsed.errors && parsed.errors.length) {
    throw new Error('oracle parseTemplate errors: ' + parsed.errors.map((e) => String(e)).join('; '));
  }

  const constantPool = new ng.ConstantPool();
  const out = ng.compileComponentFromMetadata(
    {
      name: className,
      isStandalone: true,
      selector,
      // Angular 21 reads meta.controlCreate.passThroughInput when !== null.
      controlCreate: null,
      host: { attributes: {}, listeners: {}, properties: {}, specialAttributes: {} },
      inputs: {},
      outputs: {},
      lifecycle: { usesOnChanges: false },
      hostDirectives: null,
      declarations: [],
      declarationListEmitMode: 0,
      defer: { dependenciesFn: null, mode: 1 },
      deps: [],
      animations: null,
      i18nUseExternalIds: false,
      isSignal: false,
      providers: null,
      queries: [],
      styles: [],
      template: parsed,
      encapsulation: ng.ViewEncapsulation.Emulated,
      exportAs: null,
      // OnPush so the oracle matches the Rust side, which sets OnPush to keep
      // changeDetection out of the emitted definition.
      changeDetection: ng.ChangeDetectionStrategy.OnPush,
      relativeContextFilePath: '',
      relativeTemplatePath: null,
      hasDirectiveDependencies: false,
      type: { value: new ng.WrappedNodeExpr(className), type: new ng.WrappedNodeExpr(className) },
      typeArgumentCount: 0,
      typeSourceSpan: null,
      usesInheritance: false,
      viewProviders: null,
      viewQueries: [],
    },
    constantPool,
    ng.makeBindingParser(),
  );

  const { Printer, Context } = makePrinter();
  const printer = new Printer();
  let code = out.expression.visitExpression(printer, new Context(false));
  for (const stmt of constantPool.statements) {
    code += '\n\n' + stmt.visitStatement(printer, new Context(false));
  }
  return code;
}

// ---------------------------------------------------------------------------
// Rust side: load the NAPI addon if it has been built.
// ---------------------------------------------------------------------------
function loadRustAddon() {
  // Prefer loading the native .node directly: the committed index.js glue is
  // stale (only re-exports `sum`), and `cargo build -p authoring_node` produces
  // a .node that natively exports the compile function. Fall back to the JS glue
  // modules in case a future `napi build` regenerates them.
  const candidates = [
    path.join(repoRoot, 'libs', 'authoring', 'node', 'authoring_node.win32-x64-msvc.node'),
    path.join(repoRoot, 'libs', 'authoring', 'node', 'index.js'),
    path.join(repoRoot, 'dist', 'authoring_node', 'index.js'),
  ];
  const attempts = [];
  for (const c of candidates) {
    const rel = path.relative(repoRoot, c);
    try {
      const mod = require(c);
      // napi-rs maps the Rust `compile_component` to JS `compileComponent`
      // (snake_case -> camelCase). Accept either name.
      const fn = mod.compileComponent || mod.compile_component;
      if (typeof fn === 'function') {
        return { ok: true, compile: fn, from: c };
      }
      attempts.push(
        `${rel}: loaded but missing compileComponent/compile_component ` +
          `(exports: ${Object.keys(mod).join(', ') || 'none'}).`,
      );
    } catch (err) {
      attempts.push(`${rel}: ${String(err && err.message ? err.message : err).split('\n')[0]}`);
    }
  }
  return {
    ok: false,
    reason:
      attempts.join('\n          ') +
      `\n          Build with: cd libs/authoring/node && cargo build -p authoring_node --release` +
      `\n          then copy target/release/authoring_node.dll to ` +
      `libs/authoring/node/authoring_node.win32-x64-msvc.node`,
  };
}

// ---------------------------------------------------------------------------
// Normalisation: make the two emitters comparable.
//   - unify the import prefix (Rust may emit bare ɵɵ refs or a different alias)
//   - drop the definition's `type:` line (Rust emits a TS type the oracle omits)
//   - strip all whitespace
//
// NOTE: a previous revision also "loosely sorted" the argument list of every
// instruction call. That normalization was REMOVED: instruction argument ORDER
// is semantically load-bearing in Ivy (e.g. ɵɵrepeaterCreate's positional
// template/decls/vars/track args, ɵɵconditional operands), so sorting args could
// silently mask a real arg-order lowering divergence. Verified empirically that
// removing the sort leaves the pass/diff set unchanged (10 PASS / 5 DIFF), i.e.
// it was masking nothing today — but it is a latent hazard, so it is gone.
// ---------------------------------------------------------------------------
// Hoist every named `function NAME(...) {...}` declaration out of the definition
// body into a sorted appendix, replacing each in place with NOTHING.
//
// WHY this is a false-diff fix and NOT masking: the oracle printer emits each nested
// view's template function (`*_Conditional_*`, `*_For_*`, `*_ng_template_*`) as a
// SEPARATE trailing module-scope statement (from constantPool.statements), appended
// after the `ɵɵdefineComponent({...})` expression. The Rust emitter instead emits
// the very same function declaration INLINE, nested inside the parent `_Template`
// function body, right before the `if (rf & 1)` block. Both are hoisted function
// declarations reachable at the `ɵɵconditionalCreate(.., FN, ..)` / `ɵɵrepeaterCreate`
// reference site; only the textual PLACEMENT differs (statement-position artifact,
// the same category as the documented expression-vs-statement / trailing-`;` cases).
//
// Crucially the function BODIES travel intact into the appendix, so any real
// divergence inside a nested view (different instructions, different fn NAME, an
// extra/missing branch) still produces a different appendix and is reported. Verified
// empirically: @if and @for collapse to PASS after hoisting, while
// control-flow-if-elseif-else (chained ɵɵconditionalCreate vs ɵɵconditionalBranchCreate)
// and nested-for-in-if (nested-fn naming scheme) STILL DIFF — i.e. it masks nothing.
function hoistNestedFns(code) {
  let s = code;
  const bodies = [];
  // Repeatedly pull the INNERMOST named function (one containing no further nested
  // `function NAME(` declaration) until none remain. Brace-balanced extraction.
  for (;;) {
    const re = /function\s+[A-Za-z0-9_$]+\s*\([^)]*\)\s*\{/g;
    let m;
    let pulled = false;
    while ((m = re.exec(s))) {
      const open = s.indexOf('{', m.index);
      let depth = 0;
      let k = open;
      for (; k < s.length; k++) {
        const ch = s[k];
        if (ch === '{') depth++;
        else if (ch === '}') {
          depth--;
          if (depth === 0) { k++; break; }
        }
      }
      const block = s.slice(m.index, k);
      // innermost == its body holds no further function declaration
      if (!/function\s+[A-Za-z0-9_$]+\s*\(/.test(block.slice(block.indexOf('{') + 1))) {
        bodies.push(block.replace(/\s+/g, ''));
        s = s.slice(0, m.index) + s.slice(k);
        pulled = true;
        break;
      }
    }
    if (!pulled) break;
  }
  bodies.sort();
  // The Rust emitter terminates the defineComponent({...}) STATEMENT with a `;`
  // while the oracle emits it as a bare expression. Once the nested template
  // functions are lifted out, that `;` may sit mid-string (immediately before the
  // appendix) rather than at end-of-string, so the later /;$/ strip below would
  // miss it. Drop a single trailing `;` from the skeleton here (the same
  // expression-vs-statement artifact already documented for the no-function case).
  s = s.replace(/;\s*$/, '');
  // Appendix is order-independent and whitespace-free; join with a delimiter.
  return s + (bodies.length ? 'FNS' + bodies.join('|') : '');
}

function normalize(code) {
  let s = code;
  // Drop any `import ... from '...';` preamble. The oracle printer emits only the
  // bare `ɵɵdefineComponent({...})` expression, whereas the Rust emitter prepends
  // `import * as i0 from "@angular/core";`. Strip imports so both sides start at
  // the definition.
  s = s.replace(/import[^;]*;/g, '');
  // Unify import alias: i0.ɵɵfoo / i1.ɵɵfoo -> ɵɵfoo
  s = s.replace(/\bi\d+\./g, '');
  // Unify any leftover "core."/"r3." style prefixes the Rust emitter might use.
  s = s.replace(/\b(core|r3|ng)\.(?=ɵ)/g, '');
  // Remove a trailing `type:` metadata entry the oracle/Rust may disagree on.
  s = s.replace(/,?\s*type:\s*[^,}]+/g, '');
  // Strip string-quote style differences: normalise '...' and "..." -> "..."
  s = s.replace(/'([^'\\]*)'/g, '"$1"');
  // Hoist nested view template functions out to a sorted appendix so the oracle's
  // trailing-statement placement and Rust's inline-nested placement compare equal.
  // (Done before whitespace collapse so brace balancing is reliable.)
  s = hoistNestedFns(s);
  // Collapse all whitespace.
  s = s.replace(/\s+/g, '');
  // Drop a single trailing statement terminator. The oracle printer emits the
  // bare `ɵɵdefineComponent({...})` expression (no `;`), whereas the Rust
  // emitter wraps it as a complete statement ending in `;`. This is an
  // expression-vs-statement emit artifact, not a template-lowering divergence,
  // so strip a lone trailing semicolon before comparing.
  s = s.replace(/;$/, '');
  return s;
}

function firstDiff(a, b) {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) {
      const start = Math.max(0, i - 30);
      return {
        index: i,
        oracle: a.slice(start, i + 30),
        rust: b.slice(start, i + 30),
      };
    }
  }
  if (a.length !== b.length) {
    return {
      index: n,
      oracle: a.slice(Math.max(0, n - 30)),
      rust: b.slice(Math.max(0, n - 30)),
    };
  }
  return null;
}

// ---------------------------------------------------------------------------
// Run.
// ---------------------------------------------------------------------------
function main() {
  const rust = loadRustAddon();
  console.log('Treaty render3 <-> @angular/compiler parity harness');
  console.log('='.repeat(64));
  if (rust.ok) {
    console.log(`Rust addon: LOADED from ${path.relative(repoRoot, rust.from)}`);
  } else {
    console.log('Rust addon: NOT AVAILABLE');
    console.log('  reason: ' + rust.reason);
  }
  console.log('');

  let pass = 0;
  let diff = 0;
  let oracleOnly = 0;

  for (const fx of FIXTURES) {
    console.log('-'.repeat(64));
    console.log(`Fixture: ${fx.id}`);
    console.log(`  template: ${fx.template}`);

    let oracleCode;
    try {
      oracleCode = compileWithOracle(fx);
    } catch (err) {
      console.log(`  ORACLE ERROR: ${err && err.message ? err.message : err}`);
      diff++;
      continue;
    }

    if (!rust.ok) {
      oracleOnly++;
      console.log('  RESULT: ORACLE-ONLY (Rust addon unavailable)');
      console.log('  oracle (normalized, first 200 chars):');
      console.log('    ' + normalize(oracleCode).slice(0, 200));
      continue;
    }

    let rustResult;
    try {
      rustResult = rust.compile(fx.template, fx.selector, fx.className);
    } catch (err) {
      console.log(`  RUST ERROR: ${err && err.message ? err.message : err}`);
      diff++;
      continue;
    }
    if (rustResult.errors && rustResult.errors.length) {
      console.log(`  RUST DIAGNOSTICS: ${rustResult.errors.join('; ')}`);
    }

    const nOracle = normalize(oracleCode);
    const nRust = normalize(rustResult.code);
    const d = firstDiff(nOracle, nRust);
    if (d === null) {
      pass++;
      console.log('  RESULT: PASS');
    } else {
      diff++;
      console.log('  RESULT: DIFF');
      console.log(`  first divergence at normalized index ${d.index}`);
      console.log('    oracle: ...' + d.oracle);
      console.log('    rust:   ...' + d.rust);
    }
  }

  console.log('='.repeat(64));
  console.log(`Summary: ${pass} PASS, ${diff} DIFF, ${oracleOnly} ORACLE-ONLY (of ${FIXTURES.length})`);
  // Non-zero exit only when an actual DIFF/error occurred (not for oracle-only).
  process.exit(diff > 0 ? 1 : 0);
}

main();
