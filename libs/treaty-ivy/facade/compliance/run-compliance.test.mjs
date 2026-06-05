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
} = await import('./run-compliance.mjs');

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
