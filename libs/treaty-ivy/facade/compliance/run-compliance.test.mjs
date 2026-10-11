// @ts-check
/**
 * Focused unit tests for the compliance harness's multi-class define-block SELECTION.
 *
 * A single source file that declares several classes emits several `ɵɵdefine*({...})`
 * blocks. The per-case golden is anchored on exactly ONE class (e.g. the consuming
 * `MyApp`), while our emit may declare a helper/child class (e.g. `SimpleComponent`)
 * FIRST. The harness must compare the golden's anchor class against OUR block for the
 * SAME class — selected by `type:`/`selectors:` — not blindly the first block.
 *
 * Run:  node --test libs/treaty-ivy/facade/compliance/run-compliance.test.mjs
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

process.env.COMPLIANCE_NO_MAIN = '1';
const {
  canonicalize,
  extractAllBlocksOfKind,
  anchorTypeOfBlock,
  anchorSelectorOfBlock,
  selectMatchingBlock,
  partialPairsOf,
  partialCategory,
  runPartialCase,
  runPartialBucket,
} = await import('./run-compliance.mjs');
const fsSync = await import('node:fs');
const osMod = await import('node:os');
const pathMod = await import('node:path');

const EMIT = `
SimpleComponent.ɵcmp = i0.ɵɵdefineComponent({
  type: SimpleComponent,
  selectors: [["simple"]],
  decls: 2, vars: 0
});
MyApp.ɵcmp = i0.ɵɵdefineComponent({
  type: MyApp,
  selectors: [["my-app"]],
  decls: 2, vars: 0,
  dependencies: [SimpleComponent]
});
`;

test('extractAllBlocksOfKind returns every define block in source order', () => {
  const blocks = extractAllBlocksOfKind(EMIT, 'ɵɵdefineComponent');
  assert.equal(blocks.length, 2);
  assert.equal(anchorTypeOfBlock(blocks[0]), 'SimpleComponent');
  assert.equal(anchorTypeOfBlock(blocks[1]), 'MyApp');
});

test('anchorTypeOfBlock reads the type: identifier', () => {
  assert.equal(anchorTypeOfBlock('({ type: MyApp, selectors: [["x"]] })'), 'MyApp');
  assert.equal(anchorTypeOfBlock('({ selectors: [["x"]] })'), null);
});

test('anchorSelectorOfBlock reads + canonicalises the selectors array', () => {
  assert.equal(
    anchorSelectorOfBlock('({ type: A, selectors: [["my-app"]] })'),
    anchorSelectorOfBlock('({ type: B, selectors: [["my-app"]] })'),
  );
  assert.notEqual(
    anchorSelectorOfBlock('({ selectors: [["my-app"]] })'),
    anchorSelectorOfBlock('({ selectors: [["other"]] })'),
  );
});

test('selectMatchingBlock picks OUR block matching the golden anchor type, not the first', () => {
  const golden = '({ type: MyApp, selectors: [["my-app"]], decls: 2 })';
  const picked = selectMatchingBlock(EMIT, 'ɵɵdefineComponent', golden);
  assert.equal(anchorTypeOfBlock(picked), 'MyApp');
});

test('selectMatchingBlock falls back to selectors when type identifiers differ', () => {
  const golden = '({ type: Renamed, selectors: [["my-app"]], decls: 2 })';
  const picked = selectMatchingBlock(EMIT, 'ɵɵdefineComponent', golden);
  assert.equal(anchorTypeOfBlock(picked), 'MyApp');
});

test('selectMatchingBlock degrades to the single block for a single-class file', () => {
  const single = `X.ɵcmp = i0.ɵɵdefineComponent({ type: X, selectors: [["x"]] });`;
  const picked = selectMatchingBlock(single, 'ɵɵdefineComponent', '({ type: X })');
  assert.equal(anchorTypeOfBlock(picked), 'X');
});

// ---------------------------------------------------------------------------
// Generated-temporary NAME fold: equivalent spellings of the same generated local
// must canonicalise to the SAME token, while genuinely-different identifiers and
// load-bearing instruction args must NOT.
// ---------------------------------------------------------------------------

test('contFlow temp: golden $..._contFlowTmp$ folds to our unwrapped tmp_<level>_<index>', () => {
  const golden = canonicalize(
    'let $MyApp_contFlowTmp$; ɵɵconditional(($MyApp_contFlowTmp$ = ctx.count) === 0 ? 2 : $MyApp_contFlowTmp$ === 1 ? 3 : 4);',
  );
  const port = canonicalize(
    'let tmp_0_0; i0.ɵɵconditional((tmp_0_0 = ctx.count) === 0 ? 2 : tmp_0_0 === 1 ? 3 : 4);',
  );
  assert.equal(port, golden);
});

test('contFlow temp fold is NAME-only: a different conditional case slot still diverges', () => {
  const golden = canonicalize('ɵɵconditional(($MyApp_contFlowTmp$ = ctx.count) === 0 ? 2 : 4);');
  const corrupted = canonicalize('ɵɵconditional((tmp_0_0 = ctx.count) === 0 ? 7 : 4);');
  assert.notEqual(corrupted, golden); // slot 2 vs 7 must NOT be folded away
});

test('spread local: golden <base>_r<N> folds to our $<base>_<N>$ (pureFunction-assigned)', () => {
  const golden = canonicalize(
    'const simple_r1 = $r3$.ɵɵpureFunction1(4, $c0$, ctx.foo); ɵɵtextInterpolate1(" ", simple_r1, " ");',
  );
  const port = canonicalize(
    'const $simple_1$ = i0.ɵɵpureFunction1(4, $c0$, ctx.foo); ɵɵtextInterpolate1(" ", $simple_1$, " ");',
  );
  assert.equal(port, golden);
});

test('spread local fold preserves the base name: distinct locals stay DISTINCT', () => {
  // Two distinct spread locals must NOT collapse to one token (else a swapped read passes).
  // The fold engages off the `const <local> = ɵɵpureFunction` declaration, so a realistic
  // block carries the decls; reads of each local then keep DISTINCT base-name tokens.
  const block = canonicalize(
    'const $simple_1$ = i0.ɵɵpureFunction1(4, $c0$, ctx.foo);' +
      'const $otherEntries_2$ = i0.ɵɵpureFunction1(6, $c1$, ctx.foo);' +
      'ɵɵtextInterpolate2(" ", $simple_1$, " ", $otherEntries_2$);',
  );
  assert.ok(/simple_R/.test(block), 'simple local keeps its base name');
  assert.ok(/otherEntries_R/.test(block), 'otherEntries local keeps its base name');
  // The interpolation reads the two locals as DISTINCT tokens (a swap would diverge).
  assert.ok(/ɵɵtextInterpolate2\("",simple_R,"",otherEntries_R\)/.test(block));
});

test('spread local fold is NAME-only: a different pureFunction slot index still diverges', () => {
  const golden = canonicalize('const simple_r1 = $r3$.ɵɵpureFunction1(4, $c0$, ctx.foo);');
  const corrupted = canonicalize('const $simple_1$ = i0.ɵɵpureFunction1(99, $c0$, ctx.foo);');
  assert.notEqual(corrupted, golden); // slot 4 vs 99 must NOT be folded away
});

test('spread fold does NOT touch non-pureFunction $<base>_<N>$ locals (storeLet/readContextLet)', () => {
  // Angular distinguishes these in the SAME golden ($value_0$ readContextLet vs $value_r0$
  // storeLet); our $value_N$ storeLet local must still collapse to the coarse ID, never <base>_R.
  const port = canonicalize('const $value_1$ = i0.ɵɵstoreLet(123); ɵɵtextInterpolate1(" ", $value_1$, " ");');
  assert.ok(!/value_R/.test(port), 'storeLet local must not fold to value_R');
  assert.ok(/ɵɵstoreLet\(123\)/.test(port));
});

// ---------------------------------------------------------------------------
// Partial-golden bucket: which expectation files are scored, and how a case is scored.
// ---------------------------------------------------------------------------

test('partialPairsOf: no `expectations` key means the default <stem>.js golden', () => {
  const { pairs } = partialPairsOf({ inputFiles: ['basic.ts'] }, 'basic.ts');
  assert.deepEqual(pairs.map((p) => p.expected), ['basic.js']);
});

test('partialPairsOf: an expectation without `files` also takes the default golden', () => {
  const def = { inputFiles: ['a.ts'], expectations: [{ failureMessage: 'm' }] };
  assert.deepEqual(partialPairsOf(def, 'a.ts').pairs.map((p) => p.expected), ['a.js']);
});

test('partialPairsOf: local/linked variants and GOLDEN_PARTIAL.js are not the full compile', () => {
  const def = {
    inputFiles: ['c.ts'],
    expectations: [
      { files: [{ expected: 'c.local.js', generated: 'c.local.js' }] },
      { files: [{ expected: 'c.js', generated: 'c.linked.js' }] },
      { files: ['GOLDEN_PARTIAL.js'] },
      { files: ['c.js'], compilationModeFilter: ['linked compile'] },
    ],
  };
  const { pairs, counts } = partialPairsOf(def, 'c.ts');
  assert.equal(pairs.length, 0, 'none of these is the full compile golden');
  assert.equal(counts.variant, 2);
  assert.equal(counts.ngDeclare, 1);
  assert.equal(counts.notFullCompile, 1);
});

test('partialCategory names the instruction whose arguments diverge', () => {
  assert.equal(partialCategory('ɵtextInterpolate', 'x; i0.ɵɵtextInterpolate1("", '), 'missing-instruction:ɵɵtextInterpolate');
  assert.equal(partialCategory('"title"', 'i0.ɵɵattribute('), 'args-of:ɵɵattribute');
  assert.equal(partialCategory('$r3$', 'anything'), 'placeholder-binding');
  assert.equal(partialCategory('static', '\n'), 'class-shape (static ɵ members / class body)');
});

/** Write a one-case corpus on disk and score it against a fake front-end that emits `emit`. */
function scorePartial({ golden, emit, extraChecks }) {
  const dir = fsSync.mkdtempSync(pathMod.join(osMod.tmpdir(), 'partial-case-'));
  fsSync.writeFileSync(pathMod.join(dir, 'basic.ts'), 'export class C {}');
  fsSync.writeFileSync(pathMod.join(dir, 'basic.js'), golden);
  const exp = { failureMessage: 'Incorrect boundary instructions', files: ['basic.js'] };
  if (extraChecks) exp.extraChecks = extraChecks;
  const cand = {
    tc: { dir },
    id: 'fixture/partial',
    def: { description: 'fixture', inputFiles: ['basic.ts'], expectations: [exp] },
  };
  const rust = { compileSource: () => ({ code: emit, errors: [] }) };
  try {
    return runPartialCase(cand, rust);
  } finally {
    fsSync.rmSync(dir, { recursive: true, force: true });
  }
}

test('a partial golden that matches the emit in order is a PASS', () => {
  const r = scorePartial({
    golden: '$r3$.ɵɵboundaryCreate(0);\n…\n$r3$.ɵɵboundaryUpdate(0, 1);',
    emit: 'i0.ɵɵboundaryCreate(0); i0.ɵɵtext(1); i0.ɵɵboundaryUpdate(0, 1);',
  });
  assert.equal(r.status, 'pass');
});

test('NEGATIVE: a partial golden the emit does not contain is a DIFF, never a PASS', () => {
  const r = scorePartial({
    golden: '$r3$.ɵɵboundaryCreate(0);',
    emit: 'i0.ɵɵtext(0);',
  });
  assert.equal(r.status, 'diff');
  assert.equal(r.cat, 'missing-instruction:ɵɵboundaryCreate');
});

test('NEGATIVE: a wrong argument inside a partial golden is a DIFF', () => {
  const r = scorePartial({
    golden: '$r3$.ɵɵboundaryUpdate(0, $r3$.ɵɵgetBoundary(0).error === null ? 1 : 2, 1);',
    emit: 'i0.ɵɵboundaryUpdate(0, i0.ɵɵgetBoundary(0).error === null ? 2 : 1, 1);',
  });
  assert.equal(r.status, 'diff');
});

test('NEGATIVE: a failing extra check (verifyUniqueFactory) turns a textual pass into a DIFF', () => {
  const r = scorePartial({
    golden: '$r3$.ɵɵboundaryCreate(0);',
    emit: 'i0.ɵɵboundaryCreate(0); function C_Factory() {} function C_Factory() {}',
    extraChecks: [['verifyUniqueFactory', 'C']],
  });
  assert.equal(r.status, 'diff');
  assert.equal(r.cat, 'extra-check:verifyUniqueFactory');
});

test('an i18n extra check is a skip (not a pass)', () => {
  const r = scorePartial({
    golden: '$r3$.ɵɵboundaryCreate(0);',
    emit: 'i0.ɵɵboundaryCreate(0);',
    extraChecks: ['verifyUniqueConsts'],
  });
  assert.equal(r.status, 'skip');
  assert.equal(r.cat, 'partial:extra-check:i18n');
});

test('runPartialBucket counts runnable, PASS and DIFF separately from skips', () => {
  const b = runPartialBucket([], { compileSource: () => ({ code: '', errors: [] }) });
  assert.equal(b.candidates, 0);
  assert.equal(b.runnable, 0);
});
