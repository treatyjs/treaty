/**
 * FULL-BUILD e2e bootstrap (Phase 1 harness entry — NOT the app's real entry).
 *
 * The app's real `src/main.ts` bootstraps `app-root` with the full `app.routes.ts`
 * graph. This alternate entry exists ONLY for `full-build.e2e.mjs`: it builds the
 * same app shell against the subset of routes whose authoring sources the Rust
 * compiler currently lowers to VALID, parseable Ivy — and it EAGERLY references the
 * standalone JSX surfaces (`counter.tsx`, `greeting-card.tjsx`) that are not
 * otherwise reachable from a route, so the full build exercises every authoring
 * form the compiler handles correctly:
 *
 *   - JSX `.tsx`            (counter)
 *   - JSX `.tjsx`           (greeting-card)
 *   - `.treaty` SFC         (gauge, via the metrics route)
 *   - `@Component` `.ts`    (app-root, log-viewer, dashboard, metrics-panel, profile)
 *   - lazy routes + partial-compiled `@angular/*` deps (the linker de-partials them)
 *
 * The `greeter` route is deliberately omitted here because its `.treaty` SFC
 * (`greeter.treaty`) hits a Rust-compiler lowering gap (a `server { … }` block is
 * emitted verbatim instead of being extracted, producing non-parseable output).
 * That gap is reported by the harness; this entry proves everything else builds.
 */
import { provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'
import { provideRouter, type Routes } from '@angular/router'

import { AppRoot } from './app/app-root.component'

// Eagerly pull the standalone JSX authoring surfaces into the build graph so the
// full build lowers and bundles them (they are not reachable from a route). The
// `.tjsx` surface has an ambient module declaration (`@treaty/jsx/ambient`) so it
// imports with its extension; the `.tsx` surface is a native TS extension, so it
// is pulled in lazily by an extensionless dynamic import that Vite resolves to
// `counter.tsx` at build time (keeping the bootstrap typecheck-clean without
// `allowImportingTsExtensions`).
import GreetingCard from './features/greeter/greeting-card.tjsx'

// Keep the eager JSX surfaces from being tree-shaken out of the build graph.
const jsxSurfaces: unknown[] = [GreetingCard]
void import('./components/counter').then((m) => jsxSurfaces.push(m.default))
;(globalThis as unknown as { treatyJsxSurfaces?: unknown }).treatyJsxSurfaces = jsxSurfaces

// The working subset of the app's routes (mirrors app.routes.ts minus the greeter
// route, whose .treaty SFC is blocked by the reported Rust gap).
const routes: Routes = [
	{
		path: '',
		loadComponent: () => import('./components/log-viewer.component').then((m) => m.LogViewer),
	},
	{
		path: 'dashboard',
		loadComponent: () => import('./features/dashboard/dashboard.component'),
	},
	{
		path: 'profile',
		loadChildren: () => import('./features/profile/profile.routes'),
	},
	{
		path: 'metrics',
		loadComponent: () => import('./features/metrics/metrics-panel.component'),
	},
]

void bootstrapApplication(AppRoot, {
	providers: [provideZonelessChangeDetection(), provideRouter(routes)],
})
