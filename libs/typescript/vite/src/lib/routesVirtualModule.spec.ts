import { isAbsolute, resolve } from 'node:path';
import {
  TREATY_ROUTES_ID,
  RESOLVED_TREATY_ROUTES_ID,
  isTreatyRoutesId,
  generateRoutesModule,
  resetRouteGeneratorForTesting,
  type RouteGenerator,
} from './routesVirtualModule';

/**
 * A fake route generator standing in for the Rust addon. It records the `rootDir`
 * and `configJson` it was called with and returns a deterministic module + watch
 * file list, so the tests exercise the shim wiring (id matching, option →
 * config_json translation, root resolution, absolute watch-file mapping) without
 * the native binary.
 */
function makeFakeGenerator(): RouteGenerator & {
  lastRoot: string | undefined;
  lastConfig: string | undefined;
  calls: number;
} {
  return {
    lastRoot: undefined,
    lastConfig: undefined,
    calls: 0,
    generateRoutes(rootDir: string, configJson: string) {
      this.calls += 1;
      this.lastRoot = rootDir;
      this.lastConfig = configJson;
      return {
        code: 'export const routes = [];\nexport default routes;\n',
        files: ['routes/index.treaty', 'routes/blog/index.treaty'],
        watchFiles: [],
      };
    },
  };
}

afterEach(() => {
  resetRouteGeneratorForTesting(undefined);
});

describe('isTreatyRoutesId', () => {
  it('matches the bare and resolved virtual ids', () => {
    expect(isTreatyRoutesId(TREATY_ROUTES_ID)).toBe(true);
    expect(isTreatyRoutesId(RESOLVED_TREATY_ROUTES_ID)).toBe(true);
  });

  it('matches even with a bundler-appended query/hash suffix', () => {
    expect(isTreatyRoutesId(`${TREATY_ROUTES_ID}?used`)).toBe(true);
    expect(isTreatyRoutesId(`${RESOLVED_TREATY_ROUTES_ID}#x`)).toBe(true);
  });

  it('does not match unrelated ids', () => {
    expect(isTreatyRoutesId('/repo/src/routes.ts')).toBe(false);
    expect(isTreatyRoutesId('virtual:other')).toBe(false);
  });
});

describe('generateRoutesModule', () => {
  it('resolves the virtual id to the generated TS module via the addon', () => {
    const fake = makeFakeGenerator();
    resetRouteGeneratorForTesting(fake);

    const out = generateRoutesModule({ routesRoot: '/repo/app' });

    expect(out.code).toContain('export default routes');
    expect(fake.calls).toBe(1);
    // An absolute routesRoot is passed straight through.
    expect(fake.lastRoot).toBe('/repo/app');
  });

  it('resolves a relative routesRoot against cwd to an absolute path', () => {
    const fake = makeFakeGenerator();
    resetRouteGeneratorForTesting(fake);

    generateRoutesModule({ routesRoot: 'app', cwd: '/repo' });

    expect(isAbsolute(fake.lastRoot ?? '')).toBe(true);
    expect(fake.lastRoot).toBe(resolve('/repo', 'app'));
  });

  it('only serialises the knobs that were set (absent ones fall through to core defaults)', () => {
    const fake = makeFakeGenerator();
    resetRouteGeneratorForTesting(fake);

    generateRoutesModule({ routesRoot: '/repo/app' });

    expect(fake.lastConfig).toBe('{}');
  });

  it('forwards the configured knobs as a flat config_json', () => {
    const fake = makeFakeGenerator();
    resetRouteGeneratorForTesting(fake);

    generateRoutesModule({
      routesRoot: '/repo/app',
      routesDir: 'pages',
      apiDir: 'server',
      dynamicSegmentStyle: 'colon',
      federation: false,
      importBase: '@routes',
    });

    const cfg = JSON.parse(fake.lastConfig ?? '{}') as Record<string, unknown>;
    expect(cfg).toEqual({
      routesDir: 'pages',
      apiDir: 'server',
      dynamicSegmentStyle: 'colon',
      federation: false,
      importBase: '@routes',
    });
  });

  it('maps the tree-relative route files to absolute watch files against the root', () => {
    const fake = makeFakeGenerator();
    resetRouteGeneratorForTesting(fake);

    const out = generateRoutesModule({ routesRoot: '/repo/app' });

    expect(out.files).toEqual(['routes/index.treaty', 'routes/blog/index.treaty']);
    expect(out.watchFiles).toEqual([
      resolve('/repo/app', 'routes/index.treaty'),
      resolve('/repo/app', 'routes/blog/index.treaty'),
    ]);
    for (const file of out.watchFiles) {
      expect(isAbsolute(file)).toBe(true);
    }
  });

  it('throws a clear error when the addon is unavailable', () => {
    resetRouteGeneratorForTesting(null);

    expect(() => generateRoutesModule({ routesRoot: '/repo/app' })).toThrow(
      /cannot generate file routes/,
    );
  });
});
