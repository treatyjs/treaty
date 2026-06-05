/// <reference types="node" />
import { defineConfig } from 'vite';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

// Resolve the @treaty/ts-vite plugin robustly: prefer normal package resolution (when this example
// is wired into node_modules), and fall back to the built dist in the monorepo so the example builds
// even without a per-project install (the path the e2e harness uses after wiring node_modules).
const here = dirname(fileURLToPath(import.meta.url));
const req = createRequire(import.meta.url);
function loadTreatyPlugin(): () => import('vite').Plugin[] {
  try {
    return req('@treaty/ts-vite').linkAngularPartials;
  } catch {
    return req(resolve(here, '../../libs/typescript/vite/dist/index.js')).linkAngularPartials;
  }
}
const linkAngularPartials = loadTreatyPlugin();

// Linker smoke: a minimal real Angular app (bootstrapApplication + provideRouter +
// inject(PlatformLocation)) built through the real @treaty/ts-vite plugin. `linkAngularPartials()`
// links the published partial-compiled Angular libraries (ɵɵngDeclare* -> AOT ɵɵdefine*) and
// excludes @angular/compiler, so the app boots with NO JIT and NO @angular/compiler. The app's own
// component is authored in AOT Ivy form (see src/app/*.ts), so no first-party JIT is needed either.
// `minify: false` keeps the Ivy symbols readable for the e2e bundle assertions.
export default defineConfig({
  root: here,
  plugins: [linkAngularPartials()],
  build: {
    target: 'es2022',
    minify: false,
  },
});
