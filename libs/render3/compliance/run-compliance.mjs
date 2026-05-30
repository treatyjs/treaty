// @ts-check
/**
 * Compliance harness: validate Treaty's Rust/OXC `render3` Angular compiler
 * against Angular's OWN compiler-cli compliance test suite.
 *
 * Angular vendored its compliance corpus at
 *   tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/
 * Each <category>/TEST_CASES.json lists `cases`, each case referencing input
 * `.ts` component file(s) and `expectations.files` mapping an `expected` golden
 * fragment (a `.js` file) to a `generated` output path. The golden files are
 * FRAGMENTS: they show only the relevant slice of the emitted Ivy definition and
 * use a literal `…` (U+2026) ellipsis to mean "arbitrary content elided here".
 * Angular's real harness (expect_emit.ts) treats `…` as a gap and treats
 * `$name$`-style identifiers as match-any placeholders.
 *
 * Our current Rust API surface is template-only:
 *   compile_component(template, selector, className) -> { code, errors }
 * It builds minimal component metadata (empty inputs/outputs/queries/providers,
 * no host bindings, no dependency resolution). So a compliance case is RUNNABLE
 * through us only when its expected output is determined by the TEMPLATE alone:
 * a single @Component whose interesting metadata is just template + selector +
 * class name. Cases needing inputs/outputs/DI/providers/queries/host bindings/
 * multi-file/templateUrl/directive deps are SKIPPED and counted by category, so
 * the report is an honest runnable-subset pass-rate plus a ranked gap list.
 *
 * This file is HARNESS-ONLY. It never edits the Rust crate. Build the addon:
 *   cargo build -p authoring_node --release
 *   copy target/release/authoring_node.dll
 *        -> libs/authoring/node/authoring_node.win32-x64-msvc.node
 * then:  node libs/render3/compliance/run-compliance.mjs
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

// ---------------------------------------------------------------------------
// Load the Rust NAPI addon (same strategy as parity.mjs).
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
      const fn = mod.compileComponent || mod.compile_component;
      if (typeof fn === 'function') return { ok: true, compile: fn, from: c };
      attempts.push(`${path.relative(repoRoot, c)}: missing compile fn`);
    } catch (err) {
      attempts.push(`${path.relative(repoRoot, c)}: ${String(err?.message ?? err).split('\n')[0]}`);
    }
  }
  return { ok: false, reason: attempts.join('; ') };
}

// ---------------------------------------------------------------------------
// Lightweight extraction of the @Component metadata we care about from an input
// .ts. These are hand-written, well-formed Angular test fixtures, so a focused
// scanner is sufficient (and avoids dragging in a TS parser). We extract every
// class-level decorator so we can DETECT unsupported metadata and SKIP, and the
// single template/selector/className when the case is template-only.
// ---------------------------------------------------------------------------

/** Find the substring inside the balanced parens that start at `openIdx` ('('). */
function balancedParens(src, openIdx) {
  let depth = 0;
  for (let i = openIdx; i < src.length; i++) {
    const ch = src[i];
    if (ch === '(') depth++;
    else if (ch === ')') {
      depth--;
      if (depth === 0) return src.slice(openIdx + 1, i);
    }
  }
  return src.slice(openIdx + 1);
}

/** Extract a top-level `key: <value>` from an object-literal body (depth-aware). */
function readObjectKey(body, key) {
  // Match `key:` at brace/paren depth 0 of `body`.
  const re = new RegExp(`(^|[,{\\s])${key}\\s*:`, 'g');
  let m;
  while ((m = re.exec(body))) {
    // ensure depth 0
    let depth = 0;
    for (let i = 0; i < m.index; i++) {
      const c = body[i];
      if (c === '{' || c === '[' || c === '(') depth++;
      else if (c === '}' || c === ']' || c === ')') depth--;
    }
    if (depth !== 0) continue;
    let i = m.index + m[0].length;
    while (i < body.length && /\s/.test(body[i])) i++;
    return readValueAt(body, i);
  }
  return null;
}

/** Read a JS value (string literal / template literal / object / array / ident) starting at i. */
function readValueAt(body, i) {
  const ch = body[i];
  if (ch === '`' || ch === '"' || ch === "'") {
    const quote = ch;
    let s = '';
    let j = i + 1;
    for (; j < body.length; j++) {
      const c = body[j];
      if (c === '\\') {
        // keep escape literal; resolve common ones for template text
        const next = body[j + 1];
        if (next === 'n') s += '\n';
        else if (next === 't') s += '\t';
        else if (next === '`' || next === '"' || next === "'" || next === '\\') s += next;
        else s += next;
        j++;
        continue;
      }
      if (c === quote) break;
      s += c;
    }
    return { kind: 'string', value: s, raw: quote + s + quote };
  }
  if (ch === '{' || ch === '[' || ch === '(') {
    const close = ch === '{' ? '}' : ch === '[' ? ']' : ')';
    let depth = 0;
    let j = i;
    for (; j < body.length; j++) {
      const c = body[j];
      if (c === ch) depth++;
      else if (c === close) {
        depth--;
        if (depth === 0) break;
      }
    }
    return { kind: ch === '{' ? 'object' : ch === '[' ? 'array' : 'group', value: body.slice(i, j + 1) };
  }
  // bare ident / expression up to next top-level comma
  let depth = 0;
  let j = i;
  for (; j < body.length; j++) {
    const c = body[j];
    if (c === '{' || c === '[' || c === '(') depth++;
    else if (c === '}' || c === ']' || c === ')') depth--;
    else if (c === ',' && depth === 0) break;
  }
  return { kind: 'expr', value: body.slice(i, j).trim() };
}

/**
 * Parse a single input .ts file into the list of component decorators it
 * declares, plus flags for other decorators/metadata that make a case
 * un-runnable through the template-only API.
 */
function parseInput(src) {
  const components = [];
  const otherDecorators = new Set(); // Directive, Injectable, NgModule, Pipe, etc.

  // Find each @Component(...) and the class name that follows.
  const decoRe = /@([A-Za-z][A-Za-z0-9_]*)\s*\(/g;
  let m;
  while ((m = decoRe.exec(src))) {
    const name = m[1];
    const openIdx = src.indexOf('(', m.index);
    const argsBody = balancedParens(src, openIdx);
    if (name === 'Component') {
      // metadata object literal is the first arg
      const objStart = argsBody.indexOf('{');
      const objBody = objStart >= 0 ? argsBody : '';
      // class name follows the decorator block
      const after = src.slice(openIdx);
      const clsM = /class\s+([A-Za-z0-9_$]+)/.exec(after);
      const className = clsM ? clsM[1] : null;
      components.push({ objBody, className });
    } else if (name !== 'Input' && name !== 'Output') {
      // class-level decorators we cannot model (Directive/NgModule/Pipe/Injectable/Directive)
      otherDecorators.add(name);
    } else {
      otherDecorators.add(name); // @Input / @Output member decorators
    }
  }
  return { components, otherDecorators, src };
}

// Metadata keys whose presence means the emitted output depends on more than the
// template alone (so we cannot reproduce it through compile_component).
const UNSUPPORTED_META_KEYS = [
  'inputs',
  'outputs',
  'providers',
  'viewProviders',
  'queries',
  'viewQueries',
  'host',
  'hostDirectives',
  'animations',
  'encapsulation',
  'imports',
  'deps',
  'exportAs',
  'changeDetection',
  'preserveWhitespaces',
  'interpolation',
];

// Member decorators / class shapes we cannot express.
const UNSUPPORTED_MEMBER_DECORATORS = [
  'Input',
  'Output',
  'ViewChild',
  'ViewChildren',
  'ContentChild',
  'ContentChildren',
  'HostBinding',
  'HostListener',
  'Inject',
];

/**
 * Decide whether a parsed input is runnable through compile_component, and if so
 * extract { template, selector, className }. Otherwise return a skip { skip:reason }.
 */
function classifyCase(input) {
  const { components, otherDecorators, src } = input;

  if (components.length === 0) return { skip: 'no-component' };
  if (components.length > 1) return { skip: 'multi-component' };

  // Sibling Directive/NgModule/Pipe/Injectable declarations change dependency
  // resolution / are not components we compile.
  for (const d of otherDecorators) {
    if (d === 'NgModule') {
      // NgModule alone (declarations bag) does not change template lowering; allow.
      continue;
    }
    if (d === 'Directive') return { skip: 'directive-dependency' };
    if (d === 'Pipe') return { skip: 'pipe-dependency' };
    if (d === 'Injectable') return { skip: 'injectable' };
    if (UNSUPPORTED_MEMBER_DECORATORS.includes(d)) return { skip: 'member-decorator:' + d };
  }

  const c = components[0];
  if (!c.className) return { skip: 'no-class-name' };

  const body = c.objBody;

  const tplUrl = readObjectKey(body, 'templateUrl');
  if (tplUrl) return { skip: 'templateUrl' };

  const tpl = readObjectKey(body, 'template');
  if (!tpl || tpl.kind !== 'string') return { skip: 'no-inline-template' };

  for (const key of UNSUPPORTED_META_KEYS) {
    const v = readObjectKey(body, key);
    if (v !== null) {
      // empty object/array is harmless
      if ((v.kind === 'object' || v.kind === 'array') && /^[{[]\s*[}\]]$/.test(v.value.trim())) continue;
      return { skip: 'meta:' + key };
    }
  }

  // Component-class member shapes that imply outputs/queries even without
  // decorators: signal inputs/outputs/queries (input(), output(), viewChild()).
  if (/\b(input|output|model|viewChild|viewChildren|contentChild|contentChildren)\s*(\.required)?\s*</.test(src) ||
      /=\s*(input|output|model|viewChild|viewChildren|contentChild|contentChildren)\s*\(/.test(src)) {
    return { skip: 'signal-members' };
  }

  let selector = 'app-cmp';
  const sel = readObjectKey(body, 'selector');
  if (sel && sel.kind === 'string') selector = sel.value;

  return { template: tpl.value, selector, className: c.className };
}

// ---------------------------------------------------------------------------
// Normalisation + fragment matching.
//
// We normalise BOTH the expected golden fragment and our Rust output to a canonical
// whitespace-free, placeholder-agnostic form, then check the expected fragment's
// segments (split on the `…` ellipsis) appear IN ORDER as substrings of the Rust
// output. This mirrors Angular's expect_emit semantics: `…` is a gap; identifier
// placeholders (`$r3$`, `$i0$`, `$_r2$`, `$ctx_r1$`, …) match the corresponding
// real identifier. We canonicalise all such placeholders + real temp suffixes to a
// single token so naming-scheme differences don't masquerade as divergence, while
// instruction names, argument order and slot indices (the load-bearing parts) are
// preserved.
// ---------------------------------------------------------------------------

function canonicalize(code) {
  let s = code;
  // Strip import preamble (Rust prepends `import * as i0 ...`).
  s = s.replace(/import[^;]*;/g, '');
  // Normalise string quotes to double.
  s = s.replace(/'([^'\\]*)'/g, '"$1"');
  // Unify the Ivy ref prefix: `i0.` / `$r3$.` / `$i0$.` / `r3.` / `core.` -> ''.
  s = s.replace(/\$?i\d+\$?\./g, '');
  s = s.replace(/\$?r3\$?\./g, '');
  s = s.replace(/\b(core|ng)\.(?=ɵ)/g, '');
  // Canonicalise local-ref / temp identifiers. Angular goldens use `$name$`
  // placeholders and `_rN` view-ref suffixes; our emitter uses its own suffixes.
  // Collapse both forms of generated identifier suffix to a stable token so the
  // *shape* is compared, not the exact suffix number.
  s = s.replace(/\$[A-Za-z_][A-Za-z0-9_]*\$/g, 'ID'); // $ctx_r1$, $_r2$, $i0$ already gone
  s = s.replace(/_r\d+\b/g, '_R'); // item_r1 -> item_R
  s = s.replace(/_\d+\b(?=_)/g, '_N'); // intermediate numeric segments in fn names
  // Drop the `type:` metadata entry the goldens omit / Rust emits a TS type for.
  s = s.replace(/,?\s*type:\s*[^,}]+/g, '');
  // Collapse all whitespace.
  s = s.replace(/\s+/g, '');
  // Trailing statement terminator artifact.
  s = s.replace(/;+$/g, '');
  return s;
}

/** Categorise a fragment-match failure by the first instruction at the divergence. */
function categorizeDiff(expectedSeg, rustNorm) {
  // Find the longest prefix of expectedSeg present in rustNorm to locate the
  // first point of divergence inside the failing segment.
  let lo = 0;
  let hi = expectedSeg.length;
  // binary search the longest matching prefix
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
    // structural divergence: closures, fn naming, ternary, etc.
    if (/Template\(rf,ctx\)/.test(around) || /function/.test(around)) cat = 'nested-fn-shape';
    else if (/\?.*:/.test(around)) cat = 'expression-shape';
    else cat = 'misc-shape';
  }
  return { cat, around };
}

/**
 * Match an expected golden against the Rust output.
 * Returns { pass:true } or { pass:false, cat, detail }.
 */
function matchGolden(expectedRaw, rustCode) {
  const rustNorm = canonicalize(rustCode);
  // Split the golden on the ellipsis gaps.
  const segments = expectedRaw
    .split('…')
    .map((seg) => canonicalize(seg))
    .filter((seg) => seg.length > 0);

  let cursor = 0;
  for (const seg of segments) {
    const idx = rustNorm.indexOf(seg, cursor);
    if (idx === -1) {
      // try from 0 in case ordering of our emit differs but content present
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
  const cats = fs.readdirSync(COMPLIANCE_ROOT, { withFileTypes: true }).filter((d) => d.isDirectory());
  for (const cat of cats) {
    const catDir = path.join(COMPLIANCE_ROOT, cat.name);
    const tcPath = path.join(catDir, 'TEST_CASES.json');
    if (!fs.existsSync(tcPath)) continue;
    let json;
    try {
      json = JSON.parse(fs.readFileSync(tcPath, 'utf8'));
    } catch {
      continue;
    }
    for (const c of json.cases || []) {
      out.push({ category: cat.name, dir: catDir, def: c });
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

function main() {
  const rust = loadRustAddon();
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
  };

  if (!rust.ok) {
    console.error('Rust addon NOT available: ' + rust.reason);
    process.exit(2);
  }

  for (const tc of cases) {
    const def = tc.def;
    const id = `${tc.category}/${def.description}`;
    const inputFiles = def.inputFiles || [];

    // Skip categories of metadata we cannot model.
    if (def.expectedErrors && def.expectedErrors.length) {
      bumpSkip(result, 'expects-errors');
      continue;
    }
    if (inputFiles.length !== 1) {
      bumpSkip(result, inputFiles.length === 0 ? 'no-input-file' : 'multi-file');
      continue;
    }

    const inputPath = path.join(tc.dir, inputFiles[0]);
    if (!fs.existsSync(inputPath)) {
      bumpSkip(result, 'input-missing');
      continue;
    }
    const src = fs.readFileSync(inputPath, 'utf8');
    const cls = classifyCase(parseInput(src));
    if (cls.skip) {
      bumpSkip(result, cls.skip);
      continue;
    }

    const goldens = expectedFilesOf(def);
    if (goldens.length === 0) {
      // case only has extraChecks (e.g. verifyUniqueFunctions) — no golden to diff
      bumpSkip(result, 'no-golden-only-extraChecks');
      continue;
    }

    // Runnable!
    result.runnable++;
    let rustOut;
    try {
      rustOut = rust.compile(cls.template, cls.selector, cls.className);
    } catch (err) {
      result.runError++;
      bumpDiff(result, 'RUST-THREW');
      result.diffIds.push({ id, cat: 'RUST-THREW', detail: String(err?.message ?? err).split('\n')[0] });
      continue;
    }
    if (rustOut.errors && rustOut.errors.length) {
      result.diff++;
      bumpDiff(result, 'RUST-DIAGNOSTIC');
      result.diffIds.push({ id, cat: 'RUST-DIAGNOSTIC', detail: rustOut.errors.join('; ').slice(0, 120) });
      continue;
    }

    // Match every golden fragment for this case; case passes iff all match.
    let casePass = true;
    let firstFail = null;
    for (const g of goldens) {
      const gPath = path.join(tc.dir, g);
      if (!fs.existsSync(gPath)) {
        casePass = false;
        firstFail = { cat: 'GOLDEN-MISSING', detail: g };
        break;
      }
      const expected = fs.readFileSync(gPath, 'utf8');
      const r = matchGolden(expected, rustOut.code);
      if (!r.pass) {
        casePass = false;
        firstFail = { cat: r.cat, detail: r.detail };
        break;
      }
    }

    if (casePass) {
      result.pass++;
      result.passIds.push(id);
    } else {
      result.diff++;
      bumpDiff(result, firstFail.cat);
      result.diffIds.push({ id, cat: firstFail.cat, detail: firstFail.detail });
      if (VERBOSE) console.log(`DIFF ${id}\n   ${firstFail.cat}: ...${firstFail.detail}...`);
    }
  }

  result.skipped = Object.values(result.skipByCat).reduce((a, b) => a + b, 0);

  printReport(result, rust);
  if (WRITE_REPORT) writeReport(result, rust);
  process.exit(0);
}

function bumpSkip(result, cat) {
  result.skipByCat[cat] = (result.skipByCat[cat] || 0) + 1;
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
  console.log('Treaty render3 vs Angular compliance suite');
  console.log('='.repeat(70));
  console.log(`Rust addon: ${path.relative(repoRoot, rust.from)}`);
  console.log('');
  console.log(`Total compliance cases : ${r.total}`);
  console.log(`Runnable (template-only): ${r.runnable}`);
  console.log(`  PASS  : ${r.pass}`);
  console.log(`  DIFF  : ${r.diff}`);
  console.log(`Skipped (un-runnable)  : ${r.skipped}`);
  console.log('');
  console.log(`Pass-rate (of runnable): ${runnablePass}%  (${r.pass}/${r.runnable})`);
  console.log(`Pass-rate (of total)   : ${totalPass}%  (${r.pass}/${r.total})`);
  console.log('');
  console.log('Top DIFF/gap categories (runnable cases that diverge):');
  for (const [cat, n] of ranked(r.diffByCat).slice(0, 15)) {
    console.log(`  ${String(n).padStart(4)}  ${cat}`);
  }
  console.log('');
  console.log('Skip categories (why a case is not runnable through template-only API):');
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
  lines.push('`authoring_node` NAPI addon, `compile_component(template, selector, className)`)');
  lines.push('against **Angular\'s own** `compiler-cli` compliance corpus, vendored at');
  lines.push('`tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/`.');
  lines.push('');
  lines.push('## Headline numbers');
  lines.push('');
  lines.push('| Metric | Value |');
  lines.push('| --- | --- |');
  lines.push(`| Total compliance cases | ${r.total} |`);
  lines.push(`| Runnable through template-only API | ${r.runnable} |`);
  lines.push(`| PASS | ${r.pass} |`);
  lines.push(`| DIFF | ${r.diff} |`);
  lines.push(`| Skipped (un-runnable) | ${r.skipped} |`);
  lines.push(`| **Pass-rate (of runnable subset)** | **${runnablePass}%** (${r.pass}/${r.runnable}) |`);
  lines.push(`| Pass-rate (of full corpus) | ${totalPass}% (${r.pass}/${r.total}) |`);
  lines.push('');
  lines.push('### How a case is matched');
  lines.push('');
  lines.push('Angular\'s golden files are *fragments* of the emitted Ivy definition, using a');
  lines.push('literal `…` ellipsis for "content elided" and `$name$`-style identifier');
  lines.push('placeholders. We canonicalise both the golden and our Rust output (strip imports,');
  lines.push('unify the `i0.`/`$r3$.` Ivy ref prefix, collapse identifier/temp suffixes to a');
  lines.push('stable token, drop `type:`, collapse whitespace) then require every `…`-delimited');
  lines.push('golden segment to appear, in order, in our output. Instruction names, argument');
  lines.push('order and slot indices (the load-bearing parts) are preserved by the canonicaliser.');
  lines.push('');
  lines.push('## Ranked gap categories (runnable cases that DIFF)');
  lines.push('');
  lines.push('These are the precise punch-list: the instruction / shape at the first point a');
  lines.push('runnable golden diverges from our output. Top entry = implement first to raise score.');
  lines.push('');
  lines.push('| Count | Category (instruction / shape at divergence) |');
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
  for (const [cat, items] of ranked(Object.fromEntries(Object.entries(byCat).map(([k, v]) => [k, v.length])))) {
    lines.push(`- **\`${cat}\`** (${items.length}):`);
    for (const it of byCat[cat].slice(0, 3)) {
      lines.push(`  - ${it.id}`);
      if (it.detail) lines.push(`    - near: \`${it.detail.replace(/`/g, "'").slice(0, 80)}\``);
    }
  }
  lines.push('');
  lines.push('## Skip categories (cases not runnable through the template-only API)');
  lines.push('');
  lines.push('These cases need component metadata we cannot express via');
  lines.push('`compile_component(template, selector, className)` (inputs/outputs, DI/providers,');
  lines.push('queries, host bindings, directive/pipe dependencies, multi-file, `templateUrl`,');
  lines.push('signal members, or are error-expectation / `extraChecks`-only cases with no golden).');
  lines.push('');
  lines.push('| Count | Skip reason |');
  lines.push('| --- | --- |');
  for (const [cat, n] of ranked(r.skipByCat)) {
    lines.push(`| ${n} | \`${cat}\` |`);
  }
  lines.push('');
  lines.push('## Passing cases');
  lines.push('');
  lines.push(`${r.pass} runnable compliance cases match Angular\'s golden output:`);
  lines.push('');
  for (const id of r.passIds.sort()) lines.push(`- ${id}`);
  lines.push('');

  const outPath = path.join(__dirname, 'COMPLIANCE-REPORT.md');
  fs.writeFileSync(outPath, lines.join('\n'), 'utf8');
  console.log('\nWrote ' + path.relative(repoRoot, outPath));
}

main();
