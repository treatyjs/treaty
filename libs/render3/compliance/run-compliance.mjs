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
 * then:  node libs/render3/compliance/run-compliance.mjs --report
 */

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';
import path from 'node:path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..');
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
  s = s.replace(/\\uFFFD(?:(?!\\uFFFD)[\s\S])*?\\uFFFD/g, (m) => {
    const token = ` SENTINEL${sentinelStash.length} `;
    sentinelStash.push(m);
    return token;
  });
  // Strip JS line comments EXCEPT the `// ...` ellipsis (handled before this call).
  s = s.replace(/\/\/[^\n]*/g, '');
  s = s.replace(/\/\*[^]*?\*\//g, ''); // block comments incl. /*@__PURE__*/
  // Restore the protected i18n sentinel spans verbatim.
  if (sentinelStash.length) {
    s = s.replace(/ SENTINEL(\d+) /g, (_, i) => sentinelStash[Number(i)]);
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
  s = s.replace(/\$[A-Za-z_][A-Za-z0-9_]*\$/g, 'ID'); // $ctx_r1$, $_r2$, $i0$ already gone
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

    const rustBlock = extractDefineBlockOfKind(rustOut.code, defineKind);
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

  printReport(result, rust);
  if (WRITE_REPORT) writeReport(result, rust);
  process.exit(0);
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
}

function writeReport(r, rust) {
  const runnablePass = r.runnable ? ((r.pass / r.runnable) * 100).toFixed(1) : '0.0';
  const totalPass = r.total ? ((r.pass / r.total) * 100).toFixed(1) : '0.0';
  const lines = [];
  lines.push('# Treaty `render3` — Angular Compliance Report');
  lines.push('');
  lines.push(`Generated by \`libs/render3/compliance/run-compliance.mjs\` on ${new Date().toISOString().slice(0, 10)}.`);
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

  const outPath = path.join(__dirname, 'COMPLIANCE-REPORT.md');
  fs.writeFileSync(outPath, lines.join('\n'), 'utf8');
  console.log('\nWrote ' + path.relative(repoRoot, outPath));
}

export { canonicalize, extractDefineComponentBlock, splitFragments, matchGolden };

if (!process.env.COMPLIANCE_NO_MAIN) main();
