import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { defineConfig } from "vitest/config";

const pkg = (rel: string): string =>
  fileURLToPath(new URL(`../../packages/${rel}`, import.meta.url));

/**
 * Resolve the workspace `@ngx-maintenance/*` packages directly from TypeScript
 * source. The packages are not installed (no `bun install` at this Turborepo
 * root), so tests run straight against `src`.
 */
export default defineConfig({
  resolve: {
    alias: {
      "@ngx-maintenance/registry": pkg("registry/src/index.ts"),
      "@ngx-maintenance/migration-engine": pkg(
        "migration-engine/src/index.ts",
      ),
      "@ngx-maintenance/takeover": pkg("takeover/src/index.ts"),
      "@ngx-maintenance/treaty-support": pkg("treaty-support/src/index.ts"),
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
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
