import { bootstrapApplication } from '@angular/platform-browser';
import { provideRouter } from '@angular/router';
import { AppRoot } from './app/app-root.component';
import { appRoutes } from './app/app.routes';

/**
 * Real bootstrap of a minimal Angular app that consumes published, partial-compiled Angular
 * libraries (`@angular/common`, `@angular/router`, `@angular/platform-browser`). With the @treaty
 * linker active, those libraries are de-partialled to AOT `ɵɵdefine*` at build/serve time, so
 * bootstrap succeeds WITHOUT `@angular/compiler` and WITHOUT the JIT fallback.
 *
 * `provideRouter` pulls in the router's location providers - the path that crashed with
 * "needs JIT" (`_PlatformLocation`) when `@angular/common`'s partial declarations were not linked.
 */
bootstrapApplication(AppRoot, {
  providers: [provideRouter(appRoutes)],
}).catch((err: unknown) => {
  // Surface the boot error on the document so the headless e2e can detect it deterministically.
  const el = document.createElement('pre');
  el.id = 'bootstrap-error';
  el.textContent = String(err instanceof Error ? (err.stack ?? err.message) : err);
  document.body.appendChild(el);
  console.error(err);
});
