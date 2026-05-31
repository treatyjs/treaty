import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { defineConfig } from "vitest/config";

const pkg = (rel: string): string =>
  fileURLToPath(new URL(`../${rel}`, import.meta.url));

const local = (rel: string): string =>
  fileURLToPath(new URL(rel, import.meta.url));

/**
 * Resolve the workspace `@ngx-maintenance/*` packages directly from TypeScript
 * source, and stub the out-of-band `octokit` dependency. The packages are not
 * installed (no install step at this Turborepo root) and `octokit` is supplied
 * out-of-band at deploy time, so tests run straight against `src` with a local
 * `octokit` shim that the production paths never reach under test (tests inject
 * fakes; only the type/identity surface is touched).
 */
export default defineConfig({
  resolve: {
    alias: {
      "@ngx-maintenance/github-adapter": pkg("github-adapter/src/index.ts"),
      "@ngx-maintenance/migration-engine": pkg("migration-engine/src/index.ts"),
      "@ngx-maintenance/registry": pkg("registry/src/index.ts"),
      "@ngx-maintenance/staleness-detector": pkg(
        "staleness-detector/src/index.ts",
      ),
      "@ngx-maintenance/takeover": pkg("takeover/src/index.ts"),
      "@ngx-maintenance/treaty-support": pkg("treaty-support/src/index.ts"),
      octokit: local("test/octokit-stub.ts"),
    },
  },
  plugins: [
    {
      // The package sources use explicit `.js` import specifiers (NodeNext
      // style); rewrite them to the corresponding `.ts` source so Vite can load
      // them without an emit step.
      name: "ngx-maintenance-js-to-ts",
      enforce: "pre",
      resolveId(source, importer) {
        if (importer === undefined) return null;
        if (!source.startsWith(".") || !source.endsWith(".js")) return null;
        const base = fileURLToPath(
          new URL(source, `file://${importer.replace(/\\/g, "/")}`),
        );
        const tsPath = base.replace(/\.js$/, ".ts");
        return existsSync(tsPath) ? tsPath : null;
      },
    },
  ],
  test: {
    include: ["test/**/*.test.ts"],
    environment: "node",
  },
});
