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
