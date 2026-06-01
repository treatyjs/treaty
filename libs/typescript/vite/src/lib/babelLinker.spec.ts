import { createBabelLinker } from './babelLinker';
import { composeLinkers, PARTIAL_MARKER, type PartialLinker } from './linkPartial';

describe('createBabelLinker', () => {
  const linker = createBabelLinker();

  // The Angular Babel linker ships as an ESM bundle (`@angular/compiler-cli/bundles/linker/babel`).
  // `createBabelLinker` resolves it with `require`, which works in the real (Node) Vite build but
  // NOT under Jest's CommonJS module sandbox (Jest cannot `require()` an ESM file). So under this
  // test runner the backend correctly degrades to `null`; the REAL linking is asserted end-to-end
  // in examples/linker-smoke/e2e.mjs (plain Node). When the backend IS available we still verify it
  // here. This guards the "silently degraded" failure mode without coupling the unit suite to
  // Jest's ESM-require support.
  const describeWhenAvailable = linker ? describe : describe.skip;

  describeWhenAvailable('with the real backend available', () => {
    it('links a real partial @angular/common factory: ɵɵngDeclare* -> AOT ɵɵdefine*, no @angular/compiler', () => {
      const source = [
        `import * as i0 from '@angular/core';`,
        `class PlatformLocation {}`,
        `PlatformLocation.ɵfac = function PlatformLocation_Factory(t) { return new (t || PlatformLocation)(); };`,
        `PlatformLocation.ɵprov = /*@__PURE__*/ i0.${PARTIAL_MARKER}Injectable({ minVersion: "12.0.0", version: "0.0.0-PLACEHOLDER", ngImport: i0, type: PlatformLocation, providedIn: "platform" });`,
        ``,
      ].join('\n');

      const out = linker!.linkPartial(
        source,
        '/repo/node_modules/@angular/common/fesm2022/common.mjs',
      );

      expect(out.errors).toEqual([]);
      expect(out.code).not.toContain(PARTIAL_MARKER);
      expect(out.code).toContain('ɵɵdefineInjectable');
      expect(/from\s*['"]@angular\/compiler['"]/.test(out.code)).toBe(false);
      expect(/require\(\s*['"]@angular\/compiler['"]\s*\)/.test(out.code)).toBe(false);
    });

    it('reports an error (rather than throwing) for un-parseable input', () => {
      const out = linker!.linkPartial('this is ( not valid javascript', '/repo/node_modules/x/y.mjs');
      expect(out.errors.length).toBeGreaterThan(0);
    });
  });

  it('never throws at construction time (returns a linker or null)', () => {
    expect(linker === null || typeof linker.linkPartial === 'function').toBe(true);
  });
});

describe('composeLinkers', () => {
  const RESIDUAL = `i0.${PARTIAL_MARKER}Directive({});`;

  function fakeAddon(out: string, calls: { n: number }): PartialLinker {
    return {
      linkPartial(_code, _file) {
        calls.n += 1;
        return { code: out, errors: [] };
      },
    };
  }

  it('returns the addon result unchanged when no partial marker remains', () => {
    const addonCalls = { n: 0 };
    const babelCalls = { n: 0 };
    const composed = composeLinkers(
      fakeAddon('ɵɵdefineInjectable(...)', addonCalls),
      fakeAddon('SHOULD-NOT-RUN', babelCalls),
    );

    const out = composed.linkPartial('whatever', 'x.mjs');

    expect(out.code).toBe('ɵɵdefineInjectable(...)');
    expect(addonCalls.n).toBe(1);
    expect(babelCalls.n).toBe(0); // Babel not consulted - addon fully linked.
  });

  it('runs the Babel backend over the addon output when partial declarations remain', () => {
    const addonCalls = { n: 0 };
    const babelCalls = { n: 0 };
    const composed = composeLinkers(fakeAddon(RESIDUAL, addonCalls), {
      linkPartial(code) {
        babelCalls.n += 1;
        // Simulate the complete linker finishing the residual directive.
        return { code: code.replace(/ɵɵngDeclare/g, 'ɵɵdefine'), errors: [] };
      },
    });

    const out = composed.linkPartial('partial source', 'x.mjs');

    expect(addonCalls.n).toBe(1);
    expect(babelCalls.n).toBe(1);
    expect(out.code).not.toContain(PARTIAL_MARKER);
    expect(out.code).toContain('ɵɵdefineDirective');
  });

  it('short-circuits (does not call Babel) when the addon reports errors', () => {
    const babelCalls = { n: 0 };
    const composed = composeLinkers(
      { linkPartial: () => ({ code: RESIDUAL, errors: ['addon boom'] }) },
      {
        linkPartial(code) {
          babelCalls.n += 1;
          return { code, errors: [] };
        },
      },
    );

    const out = composed.linkPartial('partial source', 'x.mjs');

    expect(out.errors).toEqual(['addon boom']);
    expect(babelCalls.n).toBe(0);
  });

  it('returns the addon directly when there is no Babel backend', () => {
    const addon: PartialLinker = { linkPartial: () => ({ code: RESIDUAL, errors: [] }) };
    expect(composeLinkers(addon, null)).toBe(addon);
  });
});
