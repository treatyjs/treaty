// @ts-check
/**
 * Unit tests for the partial-golden matcher (`expect-emit.mjs`). The NEGATIVE cases matter most: a
 * wrong emit must be reported, never turned into a pass.
 *
 * Run:  node --test libs/treaty-ivy/facade/compliance/expect-emit.test.mjs
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  expectEmit,
  replaceMacros,
  stripSourceMappingAnnotations,
  verifyUniqueFunctions,
  verifyUniqueFactory,
  ExpectEmitMismatch,
  ExpectEmitInvalid,
  UnsupportedMacro,
} = await import('./expect-emit.mjs');

// ---------------------------------------------------------------------------
// Positive: Angular's rules accept a golden that is contained, in order.
// ---------------------------------------------------------------------------

test('an exact golden matches its own text', () => {
  expectEmit('i0.ɵɵtext(0, "hi");', 'i0.ɵɵtext(0, "hi");', 'exact');
});

test('whitespace between tokens is flexible; identifiers need at least one space', () => {
  expectEmit('function  f ( rf , ctx ) {', 'function f(rf, ctx) {', 'spacing');
});

test('an ellipsis skips any amount of unrelated code', () => {
  const generated = 'a(); unrelated(); b(); more(); c();';
  expectEmit(generated, 'a(); … c();', 'ellipsis');
});

test('a `// ...` line in the golden is an ellipsis too', () => {
  expectEmit('start(); middle(); end();', 'start();\n  // ...\n  end();', 'comment-ellipsis');
});

test('`// TODO` and `// NOTE` lines are removed from the golden', () => {
  expectEmit('x(1);', '// TODO: something\nx(1);\n// NOTE: other\n', 'todo-note');
});

test('a `$name$` placeholder binds to whatever identifier the emit uses', () => {
  expectEmit('i0.ɵɵtext(0); i0.ɵɵtext(1);', '$r3$.ɵɵtext(0); $r3$.ɵɵtext(1);', 'placeholder');
});

test('a placeholder captured in one chunk is reused in a later chunk', () => {
  expectEmit('const ctx_r0 = i0.ɵɵnextContext(); … ctx_r0.x;', '$v$ = $r3$.ɵɵnextContext(); … $v$.x;', 'carry');
});

test('a lowercase-then-uppercase piece requires a separator before the next token', () => {
  // `ɵɵtext` is identifier-like ending in lowercase, so the following token needs `\s+`.
  expectEmit('return ɵɵtext 0;', 'return ɵɵtext 0;', 'ident-sep');
});

// ---------------------------------------------------------------------------
// Negative: a wrong emit is detected.
// ---------------------------------------------------------------------------

test('a wrong instruction name is a mismatch that names the missing token', () => {
  assert.throws(
    () => expectEmit('i0.ɵɵtext(0);', 'i0.ɵɵtextInterpolate(0);', 'wrong-instruction'),
    // Angular's IDENTIFIER pattern takes ONE leading `ɵ`, so `ɵɵtextInterpolate` is tokenized as
    // `ɵ` + `ɵtextInterpolate`; the missing token reported is the second half.
    (err) => err instanceof ExpectEmitMismatch && err.piece === 'ɵtextInterpolate',
  );
});

test('a wrong argument is a mismatch', () => {
  assert.throws(
    () => expectEmit('i0.ɵɵtext(0, " Main ");', 'i0.ɵɵtext(0, " Other ");', 'wrong-arg'),
    ExpectEmitMismatch,
  );
});

test('tokens out of order are a mismatch', () => {
  assert.throws(() => expectEmit('b(); a();', 'a(); b();', 'order'), ExpectEmitMismatch);
});

test('a missing token after an ellipsis is a mismatch', () => {
  assert.throws(() => expectEmit('a(); b();', 'a(); … c();', 'missing'), ExpectEmitMismatch);
});

test('a placeholder that recurs with a different identifier is a mismatch', () => {
  // `$r3$` binds to `i0` first; the second use is `i1`, so the golden does not hold.
  assert.throws(
    () => expectEmit('i0.ɵɵtext(0); i1.ɵɵtext(1);', '$r3$.ɵɵtext(0); $r3$.ɵɵtext(1);', 'bind'),
    ExpectEmitMismatch,
  );
});

test('a ctx read that was not emitted is a mismatch', () => {
  assert.throws(
    () => expectEmit('i0.ɵɵtextInterpolate1("", ctx.a, "");', 'i0.ɵɵtextInterpolate1("", ctx.b, "");', 'ctx'),
    ExpectEmitMismatch,
  );
});

test('a golden token the matcher cannot tokenize is an invalid test, not a pass', () => {
  assert.throws(() => expectEmit('anything', '#', 'invalid'), ExpectEmitInvalid);
});

test('the failure message quotes the missing token and the text before it', () => {
  try {
    expectEmit('line one;\nline two;', 'line three;', 'message');
    assert.fail('expected a mismatch');
  } catch (err) {
    assert.ok(err instanceof ExpectEmitMismatch);
    assert.equal(err.piece, 'three');
    assert.match(err.message, /Failed to find "three"/);
    assert.match(err.message, /After "line"/);
  }
});

// ---------------------------------------------------------------------------
// Macros.
// ---------------------------------------------------------------------------

test('AttributeMarker members become their Angular numbers', () => {
  assert.equal(replaceMacros('[__AttributeMarker.Bindings__, __AttributeMarker.I18n__]'), '[3, 6]');
  assert.equal(replaceMacros('__AttributeMarker.NamespaceURI__'), '0');
});

test('an unknown AttributeMarker is an error, not silently kept', () => {
  assert.throws(() => replaceMacros('__AttributeMarker.Nope__'), /Unknown AttributeMarker: Nope/);
});

test('SelectorFlags and QueryFlags unions are OR-ed', () => {
  assert.equal(replaceMacros('__SelectorFlags.ELEMENT__'), '4');
  assert.equal(replaceMacros('__SelectorFlags.NOT__|__SelectorFlags.CLASS__'), '9');
  assert.equal(replaceMacros('__QueryFlags.descendants__|__QueryFlags.isStatic__'), '3');
});

test('an i18n macro is UnsupportedMacro (skipped), never evaluated to a guess', () => {
  assert.throws(() => replaceMacros("__i18nMsg__('hi', [], {}, {})"), UnsupportedMacro);
  assert.throws(() => replaceMacros("__i18nIcuMsg__('x', [])"), UnsupportedMacro);
});

test('source-map SOURCE annotations are removed and counted, keeping the generated text', () => {
  const golden = 'i0.ɵɵtext(0); // SOURCE: "a.ts" "text"\nnext();';
  const out = stripSourceMappingAnnotations(golden);
  assert.equal(out.mappingAssertions, 1);
  // Angular consumes the annotation's trailing newline (see `extractMappings`).
  assert.equal(out.expected, 'i0.ɵɵtext(0);next();');
});

// ---------------------------------------------------------------------------
// Extra checks.
// ---------------------------------------------------------------------------

test('verifyUniqueFunctions: unique names pass, a duplicate fails', () => {
  assert.equal(verifyUniqueFunctions('function A_Template() {} function B_Template() {}'), true);
  assert.equal(verifyUniqueFunctions('function A_Template() {} function A_Template() {}'), false);
  assert.equal(verifyUniqueFunctions('no functions here'), false);
});

test('verifyUniqueFactory: exactly one factory for the type', () => {
  assert.equal(verifyUniqueFactory('function Pipe_Factory() {}', 'Pipe'), true);
  assert.equal(verifyUniqueFactory('function Pipe_Factory() {} function Pipe_Factory() {}', 'Pipe'), false);
  assert.equal(verifyUniqueFactory('function Other_Factory() {}', 'Pipe'), false);
});
