// @ts-check
/**
 * Compliance harness: validate Treaty's Rust/OXC `render3` Angular compiler
 * against Angular's OWN compiler-cli compliance test suite.
 *
 * Angular vendored its compliance corpus at
 *   tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/
 * Each <category>/TEST_CASES.json lists `cases`, each case referencing input
 * `.ts` component file(s) and `expectations.files` mapping an `expected` golden
 * `.js` to a `generated` output path. The golden files are FRAGMENTS: they show
 * the relevant slice of the emitted Ivy definition and use a literal `…`
 * (U+2026) ellipsis — and, in the full-emit goldens, a `// ...` line comment —
 * to mean "arbitrary content elided here".
 *
 * The Rust API now exposes a SOURCE front-end:
 *   compile_component_source(ts_source) -> { code, errors }
 * which parses the @Component class with oxc, extracts the common-case metadata
 * (selector, inline template, inputs/outputs, standalone, changeDetection) and
 * emits the Ivy `ɵɵdefineComponent({...})`. Shapes it cannot model yet
 * (providers/viewProviders/queries/host/hostDirectives, @ViewChild &c. member
 * decorators, templateUrl, multi-class files, @Directive-only) come back as an
 * `errors` entry rather than a mis-compile, so the harness SKIPS them.
 *
 * For each case we:
 *   1. Find the single input .ts and read its SOURCE.
 *   2. Find the FULL expected golden — the per-case `.js` referenced by the case
 *      that carries a real `ɵɵdefine*` block of ANY kind (Component / Directive /
 *      NgModule / Pipe), anchoring on the highest-priority kind present (NOT
 *      GOLDEN_PARTIAL.js / ngDeclareComponent goldens, which carry no define block and
 *      are skipped as partial-only).
 *   3. compile_component_source(src). errors => SKIPPED (categorised by reason).
 *   4. FRAGMENT-MATCH our emitted define block (the SAME kind the golden anchors on)
 *      against the golden's define block: split the expected on the `…`/`// ...` ellipsis,
 *      canonicalise each fragment + our output, and require every fragment to
 *      appear IN ORDER as a substring of our normalised output. PASS iff all do.
 *
 * Report: total cases, compiled (runnable) count, PASS / DIFF / SKIPPED,
 * pass-rate of runnable, ranked DIFF gap categories (instruction / shape at the
 * first missing fragment) and ranked skip categories. Written to
 * COMPLIANCE-REPORT.md.
 *
 * This file is HARNESS-ONLY. It never edits the Rust crate. Build the addon:
 *   cargo build -p authoring_node --release
 *   copy target/release/authoring_node.dll
 *        -> libs/authoring/node/authoring_node.win32-x64-msvc.node
 * then:  node libs/treaty-ivy/facade/compliance/run-compliance.mjs --report
 */

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';
import path from 'node:path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..', '..');
const require = createRequire(import.meta.url);

const COMPLIANCE_ROOT = path.join(
  repoRoot,
  'tools',
  'angular-ref',
  'packages',
  'compiler-cli',
  'test',
  'compliance',
  'test_cases',
);

const WRITE_REPORT = process.argv.includes('--report');
const VERBOSE = process.argv.includes('--verbose');
// `--cargo-dump=<path>`: source the front-end output from a JSON dump produced by the
// treaty_ivy cargo test `corpus_dump::dump_corpus` (a map of corpus-relative input path ->
// { code, errors }) INSTEAD of the live NAPI addon. This lets the harness verify the
// score directly against a freshly-built treaty_ivy WITHOUT rebuilding the authoring_node
// addon (which links treaty_runtime — pinned during runtime work). Byte-for-byte the same
// `compile_component_source` output the addon would surface.
import {
  expectEmit,
  replaceMacros,
  stripSourceMappingAnnotations,
  verifyUniqueFunctions,
  verifyUniqueFactory,
  ExpectEmitMismatch,
  ExpectEmitInvalid,
  UnsupportedMacro,
} from './expect-emit.mjs';

// `--fail-on-diff`: exit 1 when the FULL-golden bucket has any DIFF (the CI gate). The partial bucket
// is reported but does not gate, until its remaining DIFFs are triaged.
const FAIL_ON_DIFF = process.argv.includes('--fail-on-diff');
const CARGO_DUMP_ARG = process.argv.find((a) => a.startsWith('--cargo-dump='));
const CARGO_DUMP_PATH = CARGO_DUMP_ARG ? CARGO_DUMP_ARG.slice('--cargo-dump='.length) : null;

// ---------------------------------------------------------------------------
// Front-end source: either a cargo-produced dump (--cargo-dump) or the live NAPI addon.
// ---------------------------------------------------------------------------

/** Load a cargo-produced front-end dump: { "<corpus-rel-input-path>": {code, errors} }.
 *  Returns a `compileSource(src, inputRel)` shaped like the addon's, keyed on the corpus
 *  path (the harness passes both so the dump can be looked up deterministically). */
function loadCargoDump(dumpPath) {
  let json;
  try {
    json = JSON.parse(fs.readFileSync(dumpPath, 'utf8'));
  } catch (err) {
    return { ok: false, reason: `cargo-dump ${dumpPath}: ${String(err?.message ?? err)}` };
  }
  const compileSource = (_src, inputRel) => {
    const key = inputRel.replace(/\\/g, '/');
    const hit = json[key];
    if (!hit) return { code: '', errors: ['cargo-dump: no entry for ' + key] };
    return { code: hit.code || '', errors: hit.errors || [] };
  };
  return { ok: true, compileSource, from: dumpPath, isDump: true };
}

// ---------------------------------------------------------------------------
// Load the Rust NAPI addon. We need the SOURCE front-end `compileComponentSource`
// (snake_case `compile_component_source`). Fall back to the JS glue if present.
// ---------------------------------------------------------------------------
function loadRustAddon() {
  const candidates = [
    path.join(repoRoot, 'libs', 'authoring', 'node', 'authoring_node.win32-x64-msvc.node'),
    path.join(repoRoot, 'libs', 'authoring', 'node', 'index.js'),
  ];
  const attempts = [];
  for (const c of candidates) {
    try {
      const mod = require(c);
      const fn = mod.compileComponentSource || mod.compile_component_source;
      if (typeof fn === 'function') return { ok: true, compileSource: fn, from: c };
      attempts.push(`${path.relative(repoRoot, c)}: missing compile_component_source`);
    } catch (err) {
      attempts.push(`${path.relative(repoRoot, c)}: ${String(err?.message ?? err).split('\n')[0]}`);
    }
  }
  return { ok: false, reason: attempts.join('; ') };
}

// ---------------------------------------------------------------------------
// Golden selection + Ivy define-block extraction.
// ---------------------------------------------------------------------------

// The four FULL Ivy definition kinds a compliance golden can be built around.
// A golden that contains one of these `ɵɵdefine*({...})` blocks is a FULL golden
// (the partial/ngDeclare goldens — GOLDEN_PARTIAL.js and ngDeclareComponent — carry
// none of them). render3's emitters cover every kind: ɵɵdefineComponent (view/
// compiler.rs), ɵɵdefineDirective (compile_directive_from_metadata), ɵɵdefinePipe
// (pipe_module_injector.rs) and ɵɵdefineNgModule. We rank them so that when a single
// case golden carries several kinds (e.g. an inline @Component plus a co-declared
// @Directive) we anchor on the component block, matching what the SOURCE front-end
// emits for a `@Component` input; otherwise we anchor on the first kind present.
const DEFINE_KINDS = ['ɵɵdefineComponent', 'ɵɵdefineDirective', 'ɵɵdefineNgModule', 'ɵɵdefinePipe'];

/** Extract the FIRST balanced `marker({ ... })` argument block (incl. the surrounding
 *  parens) starting at byte `from` in `code`. Returns the `({...})` slice or null. */
function extractBalancedArgs(code, marker, from = 0) {
  const at = code.indexOf(marker, from);
  if (at === -1) return null;
  const open = code.indexOf('(', at + marker.length);
  if (open === -1) return null;
  let depth = 0;
  for (let i = open; i < code.length; i++) {
    const ch = code[i];
    if (ch === '(') depth++;
    else if (ch === ')') {
      depth--;
      if (depth === 0) return code.slice(open, i + 1);
    }
  }
  return code.slice(open);
}

/** Extract the FIRST balanced `ɵɵdefineComponent({ ... })` argument block. */
function extractDefineComponentBlock(code) {
  return extractBalancedArgs(code, 'ɵɵdefineComponent');
}

/**
 * Extract a FULL Ivy define block of a SPECIFIC kind (one of DEFINE_KINDS) from a chunk
 * of emitted JS. Returns the `({...})` slice or null when that kind is absent.
 */
function extractDefineBlockOfKind(code, kind) {
  return extractBalancedArgs(code, kind);
}

/** Extract ALL balanced `marker({ ... })` argument blocks (incl. parens) in `code`, in
 *  source order. A single source file that declares several classes emits several
 *  `ɵɵdefine*({...})` blocks; the consuming golden is anchored on ONE of them. */
function extractAllBlocksOfKind(code, kind) {
  const out = [];
  let from = 0;
  for (;;) {
    const at = code.indexOf(kind, from);
    if (at === -1) break;
    const block = extractBalancedArgs(code, kind, at);
    if (!block) break;
    out.push(block);
    // Advance past this block's opening marker so the next search finds the following one.
    from = at + kind.length;
  }
  return out;
}

/** Read the `type:` class identifier a golden/emit define block is anchored on, e.g.
 *  `type: MyApp,` -> "MyApp". Returns null when absent (the block carries no `type:`). */
function anchorTypeOfBlock(block) {
  const m = /type:\s*([A-Za-z_$][\w$]*)/.exec(block);
  return m ? m[1] : null;
}

/** Read the canonical `selectors:` slice of a define block (the bracketed array literal
 *  immediately after `selectors:`), normalised, for matching a golden anchor to OUR
 *  block when the `type:` identifiers differ in name but the selector is the anchor. */
function anchorSelectorOfBlock(block) {
  const at = block.indexOf('selectors:');
  if (at === -1) return null;
  const open = block.indexOf('[', at);
  if (open === -1) return null;
  let depth = 0;
  for (let i = open; i < block.length; i++) {
    const ch = block[i];
    if (ch === '[') depth++;
    else if (ch === ']') {
      depth--;
      if (depth === 0) return canonicalize(block.slice(open, i + 1));
    }
  }
  return null;
}

/**
 * Select, from OUR full emit, the `ɵɵdefine<kind>({...})` block that corresponds to the
 * class the GOLDEN is anchored on. A multi-class source file emits several define blocks
 * (e.g. a child component declared before the consuming component); the golden is a
 * fragment of exactly one class's def. Blindly taking the FIRST block compares the
 * golden's anchor class against whichever class we happened to emit first. Instead we:
 *   1. read the golden's anchor `type:` identifier and `selectors:`,
 *   2. find OUR block whose `type:` identifier matches (exact, name-preserving), else
 *   3. fall back to OUR block whose canonical `selectors:` equals the golden's,
 *   4. and only if neither disambiguates, the first block (legacy behaviour).
 * STRICT: this is pure SELECTION among blocks WE emitted — it never alters emitted Ivy,
 * never borrows another case's golden, and degrades to the old first-block pick when the
 * file declares a single class (the common case) or the anchors are absent.
 */
function selectMatchingBlock(code, kind, goldenBlock) {
  const blocks = extractAllBlocksOfKind(code, kind);
  if (blocks.length <= 1) return blocks[0] ?? extractDefineBlockOfKind(code, kind);
  const wantType = anchorTypeOfBlock(goldenBlock);
  if (wantType) {
    const byType = blocks.find((b) => anchorTypeOfBlock(b) === wantType);
    if (byType) return byType;
  }
  const wantSel = anchorSelectorOfBlock(goldenBlock);
  if (wantSel) {
    const bySel = blocks.find((b) => anchorSelectorOfBlock(b) === wantSel);
    if (bySel) return bySel;
  }
  return blocks[0];
}

/** Which DEFINE_KINDS (if any) does this code contain a real define block for?
 *  Returns the highest-priority kind present, or null. */
function definePresentKind(code) {
  for (const kind of DEFINE_KINDS) {
    if (code.includes(kind)) return kind;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Normalisation + fragment matching.
//
// We normalise BOTH the expected golden defineComponent block and our Rust output
// to a canonical whitespace-free, placeholder-agnostic form, then check the
// expected block's segments (split on the `…`/`// ...` ellipsis) appear IN ORDER
// as substrings of the Rust output. This mirrors Angular's expect_emit semantics:
// the ellipsis is a gap; identifier placeholders (`$r3$`, `$i0$`, `$_r2$`,
// `$ctx_r1$`, …) match the corresponding real identifier. We canonicalise such
// placeholders + real temp suffixes to a single token so naming-scheme differences
// don't masquerade as divergence, while instruction names, argument order and slot
// indices (the load-bearing parts) are preserved.
// ---------------------------------------------------------------------------

function canonicalize(code) {
  let s = code;
  // Strip import preamble (Rust prepends `import * as i0 ...`).
  s = s.replace(/import[^;]*;/g, '');
  // i18n FFFD form unification: the Angular goldens spell the placeholder magic char as the
  // escaped text \\uFFFD, whereas Treaty (like @angular/compiler real output and the
  // parity oracle) writes the RAW U+FFFD code point. Fold the escaped golden spelling to the raw
  // code point up front so both sides carry the identical byte through sentinel protection and
  // the final byte compare. STRICT: a value-preserving spelling unification of the same char.
  s = s.replace(/\\uFFFD/g, '�');
  // PROTECT i18n sentinel spans before comment stripping. An i18n placeholder
  // runtime value is the magic string `�…�` (U+FFFD…U+FFFD) whose body legitimately
  // contains `/*`, `*/` and `//` byte-sequences — e.g. a TEMPLATE_TAG close is
  // `�/*3:1�` (TAG_CLOSE `/` + TEMPLATE `*`). Those are LITERAL message content, not
  // JS comments, but the block/line comment regexes below would false-match them:
  // `�/*3:1�` opens a spurious `/* … */` that swallows the next real `/* @ts-ignore */`
  // and the golden value-map/$localize span between them. We stash each `�…�` span
  // behind an inert placeholder token, strip REAL comments, then restore the spans
  // verbatim so the sentinel compares literally and only true source comments are
  // dropped. STRICT: this neither weakens comment stripping outside sentinels nor
  // alters the sentinel bytes — they round-trip unchanged.
  // The sentinel boundary in the goldens AND our emit is the LITERAL escaped
  // text `�` (backslash-u-F-F-F-D), not a raw U+FFFD codepoint. A span runs
  // from one `�` to the next; its body never contains a nested `�`.
  const sentinelStash = [];
  s = s.replace(/�(?:(?!�)[\s\S])*?�/g, (m) => {
    const token = `\x00SENTINEL${sentinelStash.length}\x00`;
    sentinelStash.push(m);
    return token;
  });
  // Strip JS line comments EXCEPT the `// ...` ellipsis (handled before this call).
  s = s.replace(/\/\/[^\n]*/g, '');
  s = s.replace(/\/\*[^]*?\*\//g, ''); // block comments incl. /*@__PURE__*/
  // Restore the protected i18n sentinel spans verbatim.
  if (sentinelStash.length) {
    s = s.replace(/\x00SENTINEL(\d+)\x00/g, (_, i) => sentinelStash[Number(i)]);
  }
  // Normalise string quotes to double.
  s = s.replace(/'([^'\\]*)'/g, '"$1"');
  // Unify the Ivy ref prefix: `i0.` / `$r3$.` / `$i0$.` / `r3.` / `core.` -> ''.
  s = s.replace(/\$?i\d+\$?\./g, '');
  s = s.replace(/\$?r3\$?\./g, '');
  s = s.replace(/\b(core|ng)\.(?=ɵ)/g, '');
  // Angular goldens reference the AttributeMarker enum by a `__AttributeMarker.X__`
  // placeholder; our emitter is FAITHFUL to Angular's REAL emitted output and writes the
  // resolved NUMERIC marker. Map each golden enum-ref to its fixed Angular enum value so
  // the golden normalises to the SAME number we emit. These are the canonical values of
  // the `AttributeMarker` enum (packages/core/src/render3/interfaces/attribute_marker.ts):
  //   NamespaceURI=0, Classes=1, Styles=2, Bindings=3, Template=4, ProjectAs=5, I18n=6.
  // STRICT: this only maps a known enum member to its exact fixed integer (provably
  // equivalent); unknown members are left intact so they cannot silently match.
  {
    const ATTR_MARKER = {
      NamespaceURI: 0,
      Classes: 1,
      Styles: 2,
      Bindings: 3,
      Template: 4,
      ProjectAs: 5,
      I18n: 6,
    };
    s = s.replace(/__AttributeMarker\.([A-Za-z]+)__/g, (m, name) =>
      Object.prototype.hasOwnProperty.call(ATTR_MARKER, name) ? String(ATTR_MARKER[name]) : m,
    );
  }
  // Angular goldens reference two further FIXED const-enums by the SAME
  // `__Enum.Member__` placeholder convention, and (unlike AttributeMarker) compose
  // them with the bitwise-OR operator `|` (e.g. `__QueryFlags.descendants__|
  // __QueryFlags.emitDistinctChangesOnly__`). Our emitter is FAITHFUL to Angular's
  // REAL emitted output and writes the single RESOLVED integer (the OR of those flags).
  // Map each known member to its exact fixed value, then fold any chain composed
  // PURELY of such resolved members into the evaluated integer so the golden
  // normalises to the SAME number we emit.
  //   QueryFlags  (packages/core/src/render3/interfaces/query.ts):
  //     none=0, descendants=1, isStatic=2, emitDistinctChangesOnly=4.
  //   SelectorFlags (packages/core/src/render3/interfaces/projection.ts):
  //     NOT=1, ATTRIBUTE=2, ELEMENT=4, CLASS=8.
  // STRICT: only a known enum member maps to its fixed integer; an UNKNOWN member
  // is left intact (so it cannot silently match), and a chain containing any
  // un-mapped member is left intact (the `|`-fold only fires on all-numeric runs
  // produced from these maps). This is provably equivalent, never a blanket strip.
  {
    const QUERY_FLAGS = {
      none: 0,
      descendants: 1,
      isStatic: 2,
      emitDistinctChangesOnly: 4,
    };
    const SELECTOR_FLAGS = {
      NOT: 1,
      ATTRIBUTE: 2,
      ELEMENT: 4,
      CLASS: 8,
    };
    // Replace a maximal `__Enum.A__|__Enum.B__|...` chain in one pass so we can
    // OR the members together iff EVERY member of the chain is known. A chain with
    // an unknown member matches the regex but, since one member fails the map, we
    // return the original text unchanged.
    const foldEnum = (enumName, table) => {
      const memberRe = `__${enumName}\\.[A-Za-z]+__`;
      const chainRe = new RegExp(`${memberRe}(?:\\|${memberRe})*`, 'g');
      const oneRe = new RegExp(`__${enumName}\\.([A-Za-z]+)__`, 'g');
      s = s.replace(chainRe, (chain) => {
        let value = 0;
        let allKnown = true;
        chain.replace(oneRe, (mm, name) => {
          if (Object.prototype.hasOwnProperty.call(table, name)) value |= table[name];
          else allKnown = false;
          return mm;
        });
        return allKnown ? String(value) : chain;
      });
    };
    foldEnum('QueryFlags', QUERY_FLAGS);
    foldEnum('SelectorFlags', SELECTOR_FLAGS);
  }
  // Canonicalise local-ref / temp identifiers. Angular goldens use `$name$`
  // placeholders and `_rN` view-ref suffixes; our emitter uses its own suffixes.
  // The golden's `$ctx$` placeholder is the template context parameter, which Angular's
  // OWN TS printer emits as the LITERAL identifier `ctx` (so does our emitter). Map it
  // to `ctx` BEFORE the generic `$name$ -> ID` collapse so the golden's $ctx$ compares
  // EQUAL to our real literal `ctx`. STRICT: only the exact `$ctx$` token.
  s = s.replace(/\$ctx\$/g, 'ctx');
  // GENERATED-TEMPORARY NAME FOLD (names only, never semantics). Two of Treaty's
  // generated locals are spelled differently from the legacy-TDB golden even though the
  // emitted Ivy is functionally identical; we fold the EQUIVALENT spellings to one canonical
  // token on BOTH the golden and our emit so a correct emit matches regardless of the
  // local-variable spelling. STRICT: each rule matches ONLY a generated-temporary name
  // pattern (never a user identifier) and preserves the base name where one exists, so two
  // genuinely-different identifiers can NEVER collapse to the same token — only the
  // index/decoration that varies between the two compilers' naming schemes is erased. The
  // load-bearing parts (instruction names, argument order, slot indices) are untouched.
  //
  // (A) Control-flow conditional temporary. Angular's `@switch`/`@if` lowering allocates a
  // single scratch local for the conditional discriminant: the golden spells it
  // `$<Component>_contFlowTmp$` or `$tmp_<level>_<index>$` (both renamable expect-emit
  // placeholders that the generic `$name$ -> ID` collapse below already maps to `ID`), while
  // Treaty emits the SAME temporary UNWRAPPED as `tmp_<level>_<index>` (e.g. `tmp_0_0`). Map
  // our unwrapped form to the SAME `ID` token. STRICT: `tmp_<digits>_<digits>` is exclusively
  // this generated scratch name (no user identifier has that shape); a self-consistent temp
  // (assignment + every read share the name) still folds to a single `ID` on both sides, so a
  // wrong discriminant read — which would carry a DIFFERENT token/expression — still diverges.
  s = s.replace(/\btmp_\d+_\d+\b/g, 'ID');
  // (B) Spread-value view-reference local (`@let x = [...]` / `{...}` array/object spread).
  // Angular's golden spells these locals `<base>_r<N>` (the `_r<N>` view-reference suffix —
  // `simple_r1`, `otherEntries_r2`, …; the `_r\d+ -> _R` rule below already canonicalises that
  // suffix to `<base>_R`), while Treaty emits the SAME local as `$<base>_<N>$` (e.g.
  // `$simple_1$`). Map our `$<base>_<N>$` spelling to the SAME `<base>_R` token — KEEPING THE
  // BASE NAME — so `$simple_1$` compares equal to the golden's `simple_r1` while staying
  // DISTINCT from `$otherEntries_2$`.
  //
  // We must NOT fold every `$<base>_<N>$` (that conflates Treaty's `@let`/storeLet &c. locals,
  // which Angular's golden deliberately distinguishes — e.g. `$value_0$` readContextLet vs
  // `$value_r0$` storeLet in the SAME golden). The spread value local is uniquely the LHS of a
  // `const <local> = ɵɵpureFunction<N>(…)` shared-pure-literal assignment, so we restrict the
  // fold to EXACTLY the base+index tokens that appear in that position, then rewrite every
  // occurrence (declaration AND each read) of those tokens to `<base>_R`. STRICT: only a
  // `ɵɵpureFunction`-assigned generated local is touched; the base name is preserved so two
  // distinct spread locals keep distinct tokens (a swapped/mis-bound read still diverges), and
  // non-spread `$<base>_<N>$` locals (storeLet/readContextLet/reference/getCurrentView/…) fall
  // through to the coarser `$name$ -> ID` collapse below exactly as before.
  {
    const spreadLocals = new Set();
    const declRe = /\$([A-Za-z_][A-Za-z0-9_]*?_\d+)\$\s*=\s*ɵɵpureFunction\d+\b/g;
    let dm;
    while ((dm = declRe.exec(s)) !== null) spreadLocals.add(dm[1]);
    for (const tok of spreadLocals) {
      const base = tok.replace(/_\d+$/, '');
      s = s.split('$' + tok + '$').join(base + '_R');
    }
  }
  // i18n const-pool name fold: our REAL i18n consts are the bare i18n_<N> translation var and
  // MSG_<SUFFIX>_<N> closure const (matching @angular/compiler). The goldens spell the SAME
  // consts either with the renamable $i18n_<N>$ / $MSG_ID_WITH_SUFFIX$ placeholders (collapsed to
  // ID by the rule just below) OR, in the control-flow-block goldens, the bare i18n_<N> literal.
  // Fold our bare real names to the SAME ID token so either spelling compares equal. STRICT:
  // only the exact i18n translation-var / closure-const tokens are touched.
  s = s.replace(/\bi18n_\d+\b/g, 'ID');
  s = s.replace(/\bMSG_[A-Z0-9_]*\d+\b/g, 'ID');
  s = s.replace(/\$[A-Za-z_][A-Za-z0-9_]*\$/g, 'ID'); // $ctx_r1$, $_r2$, $i0$ already gone
  // Angular's `ConstantPool` shared-literal references are emitted with the REAL `_cN` name
  // (`constant_pool.ts` CONSTANT_PREFIX = "_c"), while the goldens spell the SAME reference with a
  // renamable `$cN$` / `$eN_attrs$` expect-emit placeholder (already collapsed to `ID` above). Map
  // our literal `_cN` reference to the SAME `ID` token so a hoisted shared-const reference (e.g. a
  // local-ref query predicate `ɵɵviewQuery(_c0, 5)`) compares equal to the golden's `ɵɵviewQuery(
  // $e0_attrs$, 5)`. STRICT: only the exact `_c<digits>` const-reference token; the load-bearing
  // instruction name, argument order and slot indices around it are untouched.
  s = s.replace(/\b_c\d+\b/g, 'ID');
  // The query-refresh scratch temporary is emitted with the REAL identifier `_t` (Angular
  // `generate_variables` / query lowering: `let _t; ɵɵqueryRefresh(_t = ɵɵloadQuery()) && …`),
  // while the goldens spell it with the renamable `$tmp$` expect-emit placeholder (already → `ID`).
  // Map the exact `_t` token to `ID` so a query function compares equal regardless of the temp's
  // surface name. STRICT: only the standalone `_t` identifier token.
  s = s.replace(/\b_t\b/g, 'ID');
  // A `@defer` block's lazy dependency resolver is a `ConstantPool` shared FUNCTION reference,
  // emitted with the REAL deterministic name `<fullPathName>_Defer_<deferSlot>_DepsFn` (Angular
  // `resolve_defer_deps_fns.ts` → `pool.getSharedFunctionReference(ownResolverFn,
  // `${fullPathName}_Defer_${slot}_DepsFn`, /*useUniqueName*/ false)`), while the goldens spell the
  // SAME reference with the renamable `$..._DepsFn$` expect-emit placeholder (already collapsed to
  // `ID` above). This is the identical case as the `_cN` shared-LITERAL fold: map our literal pool
  // function name to the SAME `ID` token so `ɵɵdefer(4, 2, MyApp_Defer_4_DepsFn, 3)` compares equal
  // to the golden's `ɵɵdefer(4, 2, $MyApp_Defer_4_DepsFn$, 3)`. STRICT: only the exact
  // `<Ident>_Defer_<digits>_DepsFn` pool-function token (the unambiguous renamable reference); the
  // instruction name, argument order and slot indices around it are untouched.
  s = s.replace(/\b[A-Za-z_][A-Za-z0-9_]*_Defer_\d+_DepsFn\b/g, 'ID');
  s = s.replace(/_r\d+\b/g, '_R'); // item_r1 -> item_R
  s = s.replace(/_\d+\b(?=_)/g, '_N'); // intermediate numeric segments in fn names
  // Drop the `type:` metadata entry the goldens vary on / Rust emits a TS type for.
  s = s.replace(/,?\s*type:\s*[^,}]+/g, '');
  // Collapse all whitespace.
  s = s.replace(/\s+/g, '');
  // Trailing statement terminator artifact.
  s = s.replace(/;+$/g, '');
  return s;
}

/** Split an expected golden block into ellipsis-delimited fragments. Treats both the
 *  unicode `…` and the `// ...` line-comment ellipsis as gaps. */
function splitFragments(expectedBlock) {
  // Replace `// ...` (optionally `// …`) ellipsis comments with the unicode gap so a
  // single split handles both. Match the standalone ellipsis comment form.
  const withGaps = expectedBlock.replace(/\/\/\s*(\.\.\.|…)\s*/g, '…');
  return withGaps
    .split('…')
    .map((seg) => canonicalize(seg))
    .filter((seg) => seg.length > 0);
}

/** Categorise a fragment-match failure by the first instruction/shape at the divergence. */
function categorizeDiff(expectedSeg, rustNorm) {
  // Find the longest prefix of expectedSeg present in rustNorm to locate the
  // first point of divergence inside the failing segment.
  let lo = 0;
  let hi = expectedSeg.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (rustNorm.includes(expectedSeg.slice(0, mid))) lo = mid;
    else hi = mid - 1;
  }
  const around = expectedSeg.slice(Math.max(0, lo - 4), lo + 30);
  // Identify the Ivy instruction at/just after the divergence point.
  const instr = /(ɵɵ[A-Za-z0-9]+)/.exec(expectedSeg.slice(Math.max(0, lo - 25)));
  let cat;
  if (instr) cat = instr[1];
  else {
    if (/decls:|vars:|consts:/.test(around)) cat = 'def-header-counts';
    else if (/dependencies:|directives:|pipes:/.test(around)) cat = 'dependencies';
    else if (/inputs:|outputs:/.test(around)) cat = 'io-map';
    else if (/Template\(rf,ctx\)/.test(around) || /function/.test(around)) cat = 'nested-fn-shape';
    else if (/\?.*:/.test(around)) cat = 'expression-shape';
    else cat = 'misc-shape';
  }
  return { cat, around };
}

/**
 * Match an expected golden defineComponent block against the Rust output block.
 * Returns { pass:true } or { pass:false, cat, detail }.
 */
function matchGolden(expectedBlock, rustBlock) {
  const rustNorm = canonicalize(rustBlock);
  const segments = splitFragments(expectedBlock);
  if (segments.length === 0) return { pass: false, cat: 'EMPTY-GOLDEN', detail: '' };

  let cursor = 0;
  for (const seg of segments) {
    let idx = rustNorm.indexOf(seg, cursor);
    if (idx === -1) {
      // try from 0 in case our emit ORDER differs but content is present
      const anywhere = rustNorm.indexOf(seg);
      if (anywhere === -1) {
        const { cat, around } = categorizeDiff(seg, rustNorm);
        return { pass: false, cat, detail: around };
      }
      cursor = anywhere + seg.length;
    } else {
      cursor = idx + seg.length;
    }
  }
  return { pass: true };
}

// ---------------------------------------------------------------------------
// Enumerate + run.
// ---------------------------------------------------------------------------
function enumerateCases() {
  const out = [];
  const stack = [COMPLIANCE_ROOT];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      if (e.isDirectory()) stack.push(path.join(dir, e.name));
    }
    const tcPath = path.join(dir, 'TEST_CASES.json');
    if (!fs.existsSync(tcPath)) continue;
    let json;
    try {
      json = JSON.parse(fs.readFileSync(tcPath, 'utf8'));
    } catch {
      continue;
    }
    const category = path.relative(COMPLIANCE_ROOT, dir).replace(/[\\/]/g, '/');
    for (const c of json.cases || []) {
      out.push({ category, dir, def: c });
    }
  }
  return out;
}

function expectedFilesOf(caseDef) {
  // Collect every {expected} golden file path referenced by this case.
  const files = [];
  for (const exp of caseDef.expectations || []) {
    if (!exp.files) continue;
    for (const f of exp.files) {
      if (typeof f === 'string') files.push(f);
      else if (f && f.expected) files.push(f.expected);
    }
  }
  return files;
}

/**
 * Pick the FULL golden for a case from the files the case's expectations REFERENCE.
 *
 * A FULL golden is one carrying a real `ɵɵdefine*({...})` block (any of DEFINE_KINDS:
 * Component / Directive / NgModule / Pipe). The previous selection only honoured
 * `ɵɵdefineComponent`, so any case whose referenced full golden is a `@Directive`
 * (`query_in_directive.js`, `host_bindings.js`, signal-input directives, …), an
 * NgModule (`basic_linked.js`, …) or a `@Pipe` definition was mis-classified as
 * "no full golden" and SKIPPED — even though render3 has the matching emitter and the
 * full golden sits right there on disk. We now select whichever referenced file carries
 * a real define block, anchoring on the highest-priority kind present (Component first,
 * then Directive, NgModule, Pipe) when a single golden carries more than one.
 *
 * STRICT: this only widens WHICH referenced golden counts as "full" and records the
 * exact define KIND so the run loop extracts the SAME-kind block from our emit before
 * the (unchanged) fragment matcher. We never borrow a sibling case's golden, never
 * touch a GOLDEN_PARTIAL.js / ngDeclare golden, and a case whose referenced goldens
 * carry NO define block of any kind still returns null (stays skipped).
 *
 * Returns { block, file, kind } or null.
 */
function pickFullGolden(caseDir, goldens) {
  let best = null;
  let bestRank = DEFINE_KINDS.length;
  for (const g of goldens) {
    if (/GOLDEN_PARTIAL\.js$/.test(g)) continue;
    const gPath = path.join(caseDir, g);
    if (!fs.existsSync(gPath)) continue;
    const content = fs.readFileSync(gPath, 'utf8');
    const kind = definePresentKind(content);
    if (!kind) continue;
    const rank = DEFINE_KINDS.indexOf(kind);
    if (rank >= bestRank) continue;
    const block = extractDefineBlockOfKind(content, kind);
    if (!block) continue;
    best = { block, file: g, kind };
    bestRank = rank;
    if (rank === 0) break; // Component is the highest priority; nothing can beat it.
  }
  return best;
}

function main() {
  const rust = CARGO_DUMP_PATH ? loadCargoDump(CARGO_DUMP_PATH) : loadRustAddon();
  const cases = enumerateCases();

  const result = {
    total: cases.length,
    runnable: 0,
    pass: 0,
    diff: 0,
    runError: 0,
    skipped: 0,
    skipByCat: {},
    diffByCat: {},
    passIds: [],
    diffIds: [],
    skipIds: [],
    // Cases the full bucket skips for want of a full golden: scored by `runPartialBucket`.
    partialCandidates: [],
  };

  if (!rust.ok) {
    console.error('Rust addon NOT available: ' + rust.reason);
    process.exit(2);
  }

  for (const tc of cases) {
    const def = tc.def;
    const id = `${tc.category}/${def.description}`;
    const inputFiles = def.inputFiles || [];

    // Error-expectation cases have no emit to match.
    if (def.expectedErrors && def.expectedErrors.length) {
      bumpSkip(result, 'expects-errors', id);
      continue;
    }
    if (inputFiles.length !== 1) {
      bumpSkip(result, inputFiles.length === 0 ? 'no-input-file' : 'multi-input-file', id);
      continue;
    }

    const inputPath = path.join(tc.dir, inputFiles[0]);
    if (!fs.existsSync(inputPath)) {
      bumpSkip(result, 'input-missing', id);
      continue;
    }

    // Find the FULL golden — a referenced golden carrying a real `ɵɵdefine*` block of
    // ANY kind (Component / Directive / NgModule / Pipe). Skip cases whose only
    // referenced golden is partial / ngDeclare (no define block at all).
    const goldens = expectedFilesOf(def);
    const golden = pickFullGolden(tc.dir, goldens);
    if (!golden) {
      bumpSkip(result, 'no-full-golden(partial/ngDeclare-only)', id);
      result.partialCandidates.push({ tc, id, def });
      continue;
    }
    const defineKind = golden.kind; // the Ivy define kind this golden is anchored on

    // Compile the SOURCE via the Rust front-end. The corpus-relative input path is passed
    // alongside the source so the --cargo-dump loader can key on it (the live addon ignores it).
    const src = fs.readFileSync(inputPath, 'utf8');
    const inputRel = path.relative(COMPLIANCE_ROOT, inputPath).replace(/\\/g, '/');
    let rustOut;
    try {
      rustOut = rust.compileSource(src, inputRel);
    } catch (err) {
      // A native throw is a front-end limitation -> SKIP (categorised).
      bumpSkip(result, 'source-fe-threw', id);
      continue;
    }
    if (rustOut.errors && rustOut.errors.length) {
      bumpSkip(result, classifyFeError(rustOut.errors), id);
      continue;
    }
    // The golden anchors on a specific Ivy define kind; our emit must carry the SAME
    // kind for the fragment matcher to be comparing like-with-like.
    if (!rustOut.code || !rustOut.code.includes(defineKind)) {
      bumpSkip(result, `no-define-emitted(${defineKind})`, id);
      continue;
    }

    const rustBlock = selectMatchingBlock(rustOut.code, defineKind, golden.block);
    if (!rustBlock) {
      bumpSkip(result, `no-define-block(${defineKind})`, id);
      continue;
    }

    // Runnable!
    result.runnable++;
    const r = matchGolden(golden.block, rustBlock);
    if (r.pass) {
      result.pass++;
      result.passIds.push(id);
    } else {
      result.diff++;
      bumpDiff(result, r.cat);
      result.diffIds.push({ id, cat: r.cat, detail: r.detail });
      if (VERBOSE) console.log(`DIFF ${id}\n   ${r.cat}: ...${r.detail}...`);
    }
  }

  result.skipped = Object.values(result.skipByCat).reduce((a, b) => a + b, 0);
  result.partial = runPartialBucket(result.partialCandidates, rust);

  printReport(result, rust);
  if (WRITE_REPORT) writeReport(result, rust);
  process.exit(FAIL_ON_DIFF && result.diff > 0 ? 1 : 0);
}

// ---------------------------------------------------------------------------
// Partial-golden bucket. Cases with no full (`ɵɵdefine*`) golden: a template-function golden matched
// with Angular's own `expectEmit` rules (`expect-emit.mjs`), against the same front-end emit. Kept
// separate from the full bucket so the full numbers stay comparable.
// ---------------------------------------------------------------------------

/** Extra checks this bucket runs. The i18n checks need Angular's message serializer and are skipped. */
const PARTIAL_EXTRA_CHECKS = new Set(['verifyUniqueFunctions', 'verifyUniqueFactory']);
const I18N_EXTRA_CHECKS = new Set(['verifyPlaceholdersIntegrity', 'verifyUniqueConsts']);

/**
 * The expectation files of a case that Angular checks in the FULL compile and that name the full
 * compile's output, `<input stem>.js`. Local, linked and normalized variants (other `generated` names,
 * `compilationModeFilter`s without `full compile`) and `GOLDEN_PARTIAL.js` (the ngDeclare partial
 * output) are not this bucket's concern.
 */
function partialPairsOf(def, inputFile) {
  const wantGenerated = `${path.basename(inputFile).replace(/\.ts$/, '')}.js`;
  const pairs = [];
  const counts = { notFullCompile: 0, variant: 0, ngDeclare: 0, expectsErrors: 0 };
  // Angular's `parseExpectations`: no `expectations` key, or an expectation with no `files` key, means
  // the default files: every `.ts` input's `<stem>.js`, as expected and generated.
  const tsInputs = (def.inputFiles || []).filter((f) => f.endsWith('.ts') && !f.endsWith('.d.ts'));
  const defaultFiles = tsInputs.map((f) => {
    const out = f.replace(/\.ts$/, '.js');
    return { expected: out, generated: out };
  });
  const expectations =
    def.expectations === undefined
      ? [{ failureMessage: 'Incorrect generated output.', files: defaultFiles }]
      : def.expectations;
  for (const exp of expectations) {
    const filter = exp.compilationModeFilter;
    if (filter && !filter.includes('full compile')) {
      counts.notFullCompile++;
      continue;
    }
    // An expectation that expects compile errors has no emit to match.
    if (exp.expectedErrors && exp.expectedErrors.length) {
      counts.expectsErrors++;
      continue;
    }
    for (const f of exp.files === undefined ? defaultFiles : exp.files) {
      const pair = typeof f === 'string' ? { expected: f, generated: f } : f;
      if (/GOLDEN_PARTIAL\.js$/.test(pair.expected)) {
        counts.ngDeclare++;
        continue;
      }
      if (path.basename(pair.generated) !== wantGenerated) {
        counts.variant++;
        continue;
      }
      pairs.push({
        expected: pair.expected,
        extraChecks: exp.extraChecks || [],
        failureMessage: exp.failureMessage || 'Incorrect generated output',
      });
    }
  }
  return { pairs, counts };
}

/**
 * Category of the token at which a partial golden stops matching. `after` is the golden text the
 * match had consumed up to the miss, so the category names the instruction whose ARGUMENTS diverge
 * (the last `ɵɵname` before the miss), which is the most useful grouping for the template owner.
 */
function partialCategory(piece, after) {
  if (piece === '<chunk>') return 'unlocated-chunk';
  if (/^\$.*\$$/.test(piece)) return 'placeholder-binding';
  if (piece === 'static' || piece === 'class') return 'class-shape (static ɵ members / class body)';
  if (piece.startsWith('ɵ')) return 'missing-instruction:' + (piece.length > 1 ? 'ɵɵ' + piece.slice(1).replace(/^ɵ/, '') : '?');
  const instructions = after.match(/ɵɵ\w+/g);
  if (instructions) return 'args-of:' + instructions[instructions.length - 1];
  return 'token-outside-instruction';
}

/**
 * Score one partial case. Returns { status: 'pass' | 'diff' | 'skip', cat, detail, mappingAssertions }.
 * A case is a PASS only when every applicable golden file is fully matched, with no skipped check.
 */
function runPartialCase(cand, rust) {
  const { tc, id, def } = cand;
  const skip = (cat) => ({ status: 'skip', cat });
  if (def.expectedErrors && def.expectedErrors.length) return skip('partial:expects-errors');
  const inputFiles = def.inputFiles || [];
  if (inputFiles.length !== 1) return skip('partial:multi-or-no-input-file');
  const inputFile = inputFiles[0];

  const { pairs, counts } = partialPairsOf(def, inputFile);
  if (pairs.length === 0) {
    if (counts.ngDeclare) return skip('partial:ngDeclare-partial-golden-only');
    if (counts.variant) return skip('partial:variant-generated-name(local/linked/normalized)');
    if (counts.notFullCompile) return skip('partial:compilationModeFilter-excludes-full-compile');
    if (counts.expectsErrors) return skip('partial:expects-errors');
    return skip('partial:no-expectation-files');
  }

  const inputPath = path.join(tc.dir, inputFile);
  if (!fs.existsSync(inputPath)) return skip('partial:input-missing');
  const src = fs.readFileSync(inputPath, 'utf8');
  const inputRel = path.relative(COMPLIANCE_ROOT, inputPath).replace(/\\/g, '/');
  let out;
  try {
    out = rust.compileSource(src, inputRel);
  } catch {
    return skip('partial:source-fe-threw');
  }
  if (out.errors && out.errors.length) return skip('partial:' + classifyFeError(out.errors));
  if (!out.code) return skip('partial:no-emit');

  let mappingAssertions = 0;
  for (const pair of pairs) {
    const expPath = path.join(tc.dir, pair.expected);
    if (!fs.existsSync(expPath)) return skip('partial:expected-missing');
    const raw = fs.readFileSync(expPath, 'utf8');
    // A golden that carries a `ɵɵdefine*` block is a FULL golden. The full bucket selects its goldens
    // from explicitly listed files only, so a default-file full golden is reported here, unscored.
    if (definePresentKind(raw)) return skip('partial:full-define-golden(default file, not in full bucket)');
    let expected = raw;
    try {
      // Angular order: macros first, then the source-map annotations, then `expectEmit`.
      expected = replaceMacros(expected);
    } catch (err) {
      if (err instanceof UnsupportedMacro) return skip('partial:macro:' + err.macro);
      throw err;
    }
    const stripped = stripSourceMappingAnnotations(expected);
    expected = stripped.expected;
    mappingAssertions += stripped.mappingAssertions;

    try {
      expectEmit(out.code, expected, `${id}: ${pair.failureMessage}`);
    } catch (err) {
      if (err instanceof ExpectEmitMismatch) {
        // The message ends with the text the golden matched before the missing token (`After "…"`).
        const after = /After "([\s\S]*)"$/.exec(err.message)?.[1] ?? '';
        return {
          status: 'diff',
          cat: partialCategory(err.piece, after),
          detail: `missing ${JSON.stringify(err.piece)} after ${JSON.stringify(after.slice(-60))}`,
          mappingAssertions,
        };
      }
      if (err instanceof ExpectEmitInvalid) return skip('partial:golden-untokenizable');
      throw err;
    }

    for (const check of pair.extraChecks) {
      const [name, ...args] = Array.isArray(check) ? check : [check];
      if (I18N_EXTRA_CHECKS.has(name)) return skip('partial:extra-check:i18n');
      if (!PARTIAL_EXTRA_CHECKS.has(name)) return skip(`partial:extra-check:${name}`);
      const ok =
        name === 'verifyUniqueFunctions'
          ? verifyUniqueFunctions(out.code, ...args)
          : verifyUniqueFactory(out.code, ...args);
      if (!ok) return { status: 'diff', cat: `extra-check:${name}`, detail: JSON.stringify(args), mappingAssertions };
    }
  }
  return { status: 'pass', mappingAssertions };
}

function bumpMap(map, key) {
  map[key] = (map[key] || 0) + 1;
}

/** Score every partial candidate; returns the bucket summary used by the report. */
function runPartialBucket(candidates, rust) {
  const b = {
    candidates: candidates.length,
    runnable: 0,
    pass: 0,
    diff: 0,
    passWithUnverifiedMappings: 0,
    mappingAssertions: 0,
    skipByCat: {},
    diffByCat: {},
    passIds: [],
    diffIds: [],
  };
  for (const cand of candidates) {
    const r = runPartialCase(cand, rust);
    if (r.status === 'skip') {
      bumpMap(b.skipByCat, r.cat);
      continue;
    }
    b.runnable++;
    b.mappingAssertions += r.mappingAssertions || 0;
    if (r.status === 'pass') {
      b.pass++;
      if (r.mappingAssertions) b.passWithUnverifiedMappings++;
      b.passIds.push(cand.id);
    } else {
      b.diff++;
      bumpMap(b.diffByCat, r.cat);
      b.diffIds.push({ id: cand.id, cat: r.cat, detail: r.detail });
    }
  }
  b.skipped = Object.values(b.skipByCat).reduce((a, n) => a + n, 0);
  return b;
}

/** Bucket a front-end error message into a stable skip category. */
function classifyFeError(errors) {
  const msg = errors.join(' | ').toLowerCase();
  if (msg.includes('provider')) return 'fe:providers';
  if (msg.includes('queries') || msg.includes('viewchild') || msg.includes('contentchild'))
    return 'fe:queries';
  if (msg.includes('host')) return 'fe:host/hostDirectives';
  if (msg.includes('templateurl')) return 'fe:templateUrl';
  if (msg.includes('multi-class')) return 'fe:multi-class';
  if (msg.includes('directive')) return 'fe:directive-only';
  if (msg.includes('no inline string') || msg.includes('no inline'))
    return 'fe:no-inline-template';
  if (msg.includes('no @component') || msg.includes('decorated class'))
    return 'fe:no-component';
  if (msg.includes('parse error')) return 'fe:parse-error';
  return 'fe:other(' + errors[0].slice(0, 40) + ')';
}

function bumpSkip(result, cat, id) {
  result.skipByCat[cat] = (result.skipByCat[cat] || 0) + 1;
  result.skipIds.push({ id, cat });
}
function bumpDiff(result, cat) {
  result.diffByCat[cat] = (result.diffByCat[cat] || 0) + 1;
}

function ranked(map) {
  return Object.entries(map).sort((a, b) => b[1] - a[1]);
}

function printReport(r, rust) {
  const runnablePass = r.runnable ? ((r.pass / r.runnable) * 100).toFixed(1) : '0.0';
  const totalPass = r.total ? ((r.pass / r.total) * 100).toFixed(1) : '0.0';
  console.log('='.repeat(70));
  console.log('Treaty render3 vs Angular compliance suite (SOURCE front-end)');
  console.log('='.repeat(70));
  console.log(`Rust addon: ${path.relative(repoRoot, rust.from)}`);
  console.log('');
  console.log(`Total compliance cases     : ${r.total}`);
  console.log(`Compiled (runnable)        : ${r.runnable}`);
  console.log(`  PASS  : ${r.pass}`);
  console.log(`  DIFF  : ${r.diff}`);
  console.log(`Skipped (un-runnable)      : ${r.skipped}`);
  console.log('');
  console.log(`Pass-rate (of runnable)    : ${runnablePass}%  (${r.pass}/${r.runnable})`);
  console.log(`Pass-rate (of total)       : ${totalPass}%  (${r.pass}/${r.total})`);
  console.log('');
  console.log('Top DIFF/gap categories (runnable cases that diverge):');
  for (const [cat, n] of ranked(r.diffByCat).slice(0, 12)) {
    console.log(`  ${String(n).padStart(4)}  ${cat}`);
  }
  console.log('');
  console.log('Top skip categories (case not runnable through source front-end):');
  for (const [cat, n] of ranked(r.skipByCat).slice(0, 20)) {
    console.log(`  ${String(n).padStart(4)}  ${cat}`);
  }
  printPartialSummary(r.partial);
}

/** The partial-golden bucket, printed separately from the full-golden numbers above. */
function printPartialSummary(p) {
  if (!p) return;
  const pct = p.runnable ? ((p.pass / p.runnable) * 100).toFixed(1) : '0.0';
  console.log('');
  console.log('='.repeat(70));
  console.log('Partial-golden bucket (template-function goldens, Angular expectEmit rules)');
  console.log('='.repeat(70));
  console.log(`Candidates (no full golden): ${p.candidates}`);
  console.log(`Runnable                    : ${p.runnable}`);
  console.log(`  PASS  : ${p.pass}   (${pct}% of runnable)`);
  console.log(`  DIFF  : ${p.diff}`);
  console.log(`Skipped                     : ${p.skipped}`);
  console.log(`Source-map assertions not verified (in passing cases): ${p.passWithUnverifiedMappings} case(s), ${p.mappingAssertions} assertion(s) total`);
  console.log('DIFF by category:');
  for (const [cat, n] of ranked(p.diffByCat).slice(0, 25)) console.log(`  ${String(n).padStart(4)}  ${cat}`);
  console.log('Skip by reason:');
  for (const [cat, n] of ranked(p.skipByCat).slice(0, 25)) console.log(`  ${String(n).padStart(4)}  ${cat}`);
}

/** The pinned Angular commit (`tools/render3-sync/ANGULAR_REF`), or `unknown` if it is missing. */
function angularRefSha() {
  try {
    return fs.readFileSync(path.join(repoRoot, 'tools', 'render3-sync', 'ANGULAR_REF'), 'utf8').trim();
  } catch {
    return 'unknown';
  }
}

function writeReport(r, rust) {
  const runnablePass = r.runnable ? ((r.pass / r.runnable) * 100).toFixed(1) : '0.0';
  const totalPass = r.total ? ((r.pass / r.total) * 100).toFixed(1) : '0.0';
  const lines = [];
  lines.push('# Treaty `render3` — Angular Compliance Report');
  lines.push('');
  lines.push(`Generated by \`libs/treaty-ivy/facade/compliance/run-compliance.mjs\` on ${new Date().toISOString().slice(0, 10)}.`);
  lines.push('');
  lines.push(`Angular reference: \`${angularRefSha()}\` (\`tools/render3-sync/ANGULAR_REF\`), corpus \`tools/angular-ref/packages/compiler-cli/test/compliance/test_cases\`, ${r.total} cases.`);
  lines.push('');
  lines.push('This harness runs Treaty\'s Rust/OXC Angular compiler (`render3` crate, via the');
  lines.push('`authoring_node` NAPI addon\'s **source front-end**');
  lines.push('`compile_component_source(ts_source) -> { code, errors }`) against **Angular\'s own**');
  lines.push('`compiler-cli` compliance corpus, vendored at');
  lines.push('`tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/`.');
  lines.push('');
  lines.push('## Headline numbers');
  lines.push('');
  lines.push('| Metric | Value |');
  lines.push('| --- | --- |');
  lines.push(`| Total compliance cases | ${r.total} |`);
  lines.push(`| Compiled (runnable) | ${r.runnable} |`);
  lines.push(`| PASS | ${r.pass} |`);
  lines.push(`| DIFF | ${r.diff} |`);
  lines.push(`| Skipped (un-runnable) | ${r.skipped} |`);
  lines.push(`| **Pass-rate (of runnable subset)** | **${runnablePass}%** (${r.pass}/${r.runnable}) |`);
  lines.push(`| Pass-rate (of full corpus) | ${totalPass}% (${r.pass}/${r.total}) |`);
  if (r.partial) {
    const p = r.partial;
    const partialPct = p.runnable ? ((p.pass / p.runnable) * 100).toFixed(1) : '0.0';
    lines.push(`| **Partial-golden bucket** (separate; no full golden) | **${partialPct}%** PASS (${p.pass}/${p.runnable} runnable, ${p.candidates} candidates, ${p.diff} DIFF, ${p.skipped} skipped) |`);
  }
  lines.push('');
  lines.push('### How a case is run and matched');
  lines.push('');
  lines.push('1. The single input `.ts` `@Component` is read and its SOURCE passed to');
  lines.push('   `compile_component_source`. If the front-end returns an error (providers,');
  lines.push('   queries, host bindings, `templateUrl`, multi-class, `@Directive`-only, etc.)');
  lines.push('   the case is **SKIPPED** and counted by reason.');
  lines.push('2. The expected golden is the per-case `.js` that contains `ɵɵdefineComponent`');
  lines.push('   (NOT `GOLDEN_PARTIAL.js` / `ngDeclareComponent` goldens — those are skipped as');
  lines.push('   partial-only).');
  lines.push('3. We extract the balanced `ɵɵdefineComponent({...})` block from BOTH the golden');
  lines.push('   and our emit, canonicalise (strip imports/comments, unify the `i0.`/`$r3$.`');
  lines.push('   Ivy ref prefix, collapse `$name$`/`_rN` placeholders + `__AttributeMarker__`');
  lines.push('   tokens, drop `type:`, collapse whitespace), then require every `…`/`// ...`');
  lines.push('   ellipsis-delimited golden fragment to appear, IN ORDER, in our output.');
  lines.push('   Instruction names, argument order and slot indices are preserved.');
  lines.push('');
  lines.push('## Ranked gap categories (runnable cases that DIFF)');
  lines.push('');
  lines.push('The instruction / shape at the first point a runnable golden fragment is missing');
  lines.push('from our output. Top entry = implement first to raise the score.');
  lines.push('');
  lines.push('| Count | Category (instruction / shape at first missing fragment) |');
  lines.push('| --- | --- |');
  for (const [cat, n] of ranked(r.diffByCat)) {
    lines.push(`| ${n} | \`${cat}\` |`);
  }
  lines.push('');
  lines.push('### Sample diverging cases');
  lines.push('');
  const byCat = {};
  for (const d of r.diffIds) {
    (byCat[d.cat] ||= []).push(d);
  }
  for (const [cat] of ranked(Object.fromEntries(Object.entries(byCat).map(([k, v]) => [k, v.length])))) {
    lines.push(`- **\`${cat}\`** (${byCat[cat].length}):`);
    for (const it of byCat[cat].slice(0, 3)) {
      lines.push(`  - ${it.id}`);
      if (it.detail) lines.push(`    - near: \`${String(it.detail).replace(/`/g, "'").slice(0, 80)}\``);
    }
  }
  lines.push('');
  lines.push('## Skip categories (cases not runnable through the source front-end)');
  lines.push('');
  lines.push('`fe:*` reasons come from the Rust source front-end declining a metadata shape it');
  lines.push('cannot model yet; the others are corpus-level (no usable golden, multi-file, or');
  lines.push('error-expectation cases).');
  lines.push('');
  lines.push('| Count | Skip reason |');
  lines.push('| --- | --- |');
  for (const [cat, n] of ranked(r.skipByCat)) {
    lines.push(`| ${n} | \`${cat}\` |`);
  }
  lines.push('');
  lines.push('## Passing cases');
  lines.push('');
  lines.push(`${r.pass} runnable compliance cases match Angular\'s golden \`ɵɵdefineComponent\` block:`);
  lines.push('');
  for (const id of r.passIds.slice().sort()) lines.push(`- ${id}`);
  lines.push('');

  const p = r.partial;
  if (p) {
    lines.push('## Partial-golden bucket (template-function goldens)');
    lines.push('');
    lines.push('Cases with no full `ɵɵdefine*` golden: a template-function golden (for example');
    lines.push('`r3_view_compiler_boundaries/basic.js`), matched with Angular\'s own `expectEmit` rules');
    lines.push('(`compiler-cli/test/compliance/test_helpers/expect_emit.ts`, ported in `expect-emit.mjs`):');
    lines.push('`…` / `// ...` skip, `$name$` identifier placeholders, `__AttributeMarker__` and');
    lines.push('`__SelectorFlags__` / `__QueryFlags__` macros. Tokens must appear IN ORDER.');
    lines.push('');
    lines.push('Reported separately so the full-golden numbers above stay comparable. i18n macros and');
    lines.push('i18n extra checks are skipped (they need Angular\'s message serializer). The `// SOURCE:`');
    lines.push(`source-map assertions are not verified: ${p.mappingAssertions} assertion(s) in ${p.passWithUnverifiedMappings} passing case(s).`);
    lines.push('');
    lines.push('| Metric | Value |');
    lines.push('| --- | --- |');
    lines.push(`| Candidates (no full golden) | ${p.candidates} |`);
    lines.push(`| Runnable | ${p.runnable} |`);
    lines.push(`| PASS | ${p.pass} |`);
    lines.push(`| DIFF | ${p.diff} |`);
    lines.push(`| Skipped | ${p.skipped} |`);
    lines.push('');
    lines.push('### Partial DIFF categories');
    lines.push('');
    lines.push('| Count | Category (token at which the golden stops matching) |');
    lines.push('| --- | --- |');
    for (const [cat, n] of ranked(p.diffByCat)) lines.push(`| ${n} | \`${cat}\` |`);
    lines.push('');
    lines.push('### Partial skip reasons');
    lines.push('');
    lines.push('| Count | Reason |');
    lines.push('| --- | --- |');
    for (const [cat, n] of ranked(p.skipByCat)) lines.push(`| ${n} | \`${cat}\` |`);
    lines.push('');
    lines.push('### Partial DIFF cases');
    lines.push('');
    for (const d of p.diffIds.slice().sort((a, b) => a.cat.localeCompare(b.cat) || a.id.localeCompare(b.id))) {
      lines.push(`- \`${d.cat}\` — ${d.id}`);
      if (d.detail) lines.push(`  - ${String(d.detail).replace(/`/g, "'")}`);
    }
    lines.push('');
    lines.push('### Passing partial cases');
    lines.push('');
    for (const id of p.passIds.slice().sort()) lines.push(`- ${id}`);
    lines.push('');
  }

  const outPath = path.join(__dirname, 'COMPLIANCE-REPORT.md');
  fs.writeFileSync(outPath, lines.join('\n'), 'utf8');
  console.log('\nWrote ' + path.relative(repoRoot, outPath));
}

export {
  canonicalize,
  extractDefineComponentBlock,
  splitFragments,
  matchGolden,
  extractAllBlocksOfKind,
  anchorTypeOfBlock,
  anchorSelectorOfBlock,
  selectMatchingBlock,
  partialPairsOf,
  partialCategory,
  runPartialCase,
  runPartialBucket,
};

if (!process.env.COMPLIANCE_NO_MAIN) main();
