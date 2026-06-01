import {
  PARTIAL_MARKER,
  isPartialModule,
  linkPartialCode,
  resetLinkerForTesting,
  type PartialLinker,
} from './linkPartial';

/**
 * A fake linker mirroring the real addon's behaviour closely enough to exercise the wiring:
 * it rewrites the `ɵɵngDeclare*` family to `ɵɵdefine*` and reports how many calls it linked.
 */
function makeFakeLinker(): PartialLinker & { calls: number } {
  return {
    calls: 0,
    linkPartial(code: string) {
      this.calls += 1;
      return {
        code: code.replace(/ɵɵngDeclare/g, 'ɵɵdefine'),
        errors: [],
      };
    },
  };
}

const PARTIAL_NODE_MODULES_ID =
  '/repo/node_modules/@angular/common/fesm2022/common.mjs';
const PARTIAL_SOURCE = `import * as i0 from '@angular/core';\nP.ɵpipe = i0.${PARTIAL_MARKER}Pipe({ type: P, name: 'x' });\n`;

afterEach(() => {
  // Clear the memoised linker AND the module-level content cache state between tests by
  // re-priming with a fresh fake in the tests that need it.
  resetLinkerForTesting(undefined);
});

describe('isPartialModule', () => {
  it('detects a partial-compiled module under node_modules', () => {
    expect(isPartialModule(PARTIAL_NODE_MODULES_ID, PARTIAL_SOURCE)).toBe(true);
  });

  it('ignores a partial module that is NOT under node_modules (first-party source)', () => {
    expect(isPartialModule('/repo/src/app/foo.ts', PARTIAL_SOURCE)).toBe(false);
  });

  it('ignores a non-partial node_modules module', () => {
    expect(
      isPartialModule(
        '/repo/node_modules/lodash/index.js',
        'module.exports = {};',
      ),
    ).toBe(false);
  });
});

describe('linkPartialCode', () => {
  it('links a partial node_modules module: ɵɵngDeclare* -> ɵɵdefine*', () => {
    resetLinkerForTesting(makeFakeLinker());

    const out = linkPartialCode(PARTIAL_SOURCE, PARTIAL_NODE_MODULES_ID);

    expect(out).not.toBeNull();
    expect(out).toContain('ɵɵdefinePipe');
    expect(out).not.toContain(PARTIAL_MARKER);
  });

  it('passes through (returns null) for non-partial input', () => {
    resetLinkerForTesting(makeFakeLinker());

    const out = linkPartialCode(
      'export const answer = 42;',
      '/repo/node_modules/some-lib/index.js',
    );

    expect(out).toBeNull();
  });

  it('passes through (returns null) for first-party (non-node_modules) input', () => {
    resetLinkerForTesting(makeFakeLinker());

    const out = linkPartialCode(PARTIAL_SOURCE, '/repo/src/app/foo.ts');

    expect(out).toBeNull();
  });

  it('passes through (returns null) when no linker addon is available', () => {
    resetLinkerForTesting(null);

    // Use a source unique to this test: the content-keyed cache (intentionally) keeps a module
    // linked once it has been linked, so a body already linked by an earlier test would short-circuit
    // before the linker is consulted and mask the no-addon path. A never-cached body exercises it.
    const uniqueSource = `${PARTIAL_SOURCE}\n// no-addon-test-${Math.random()}`;
    const out = linkPartialCode(uniqueSource, PARTIAL_NODE_MODULES_ID);

    expect(out).toBeNull();
  });

  it('caches by content so each distinct module body is linked exactly once', () => {
    const fake = makeFakeLinker();
    resetLinkerForTesting(fake);

    // Use a content unique to this test so the module-level content cache cannot be pre-warmed.
    const uniqueSource = `${PARTIAL_SOURCE}\n// cache-test-${Math.random()}`;

    const first = linkPartialCode(uniqueSource, PARTIAL_NODE_MODULES_ID);
    const second = linkPartialCode(
      uniqueSource,
      '/repo/node_modules/@angular/common/fesm2022/other.mjs',
    );

    expect(first).toEqual(second);
    expect(fake.calls).toBe(1);
  });

  it('throws when the linker reports diagnostics', () => {
    resetLinkerForTesting({
      linkPartial() {
        return { code: '', errors: ['could not link ɵɵngDeclareInjector'] };
      },
    });

    const unique = `${PARTIAL_SOURCE}\n// error-test-${Math.random()}`;
    expect(() => linkPartialCode(unique, PARTIAL_NODE_MODULES_ID)).toThrow(
      /failed to link partial Angular module/,
    );
  });
});
