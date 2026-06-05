/**
 * NAV e2e bootstrap (`nav.e2e.mjs` harness entry — NOT the app's real entry).
 *
 * The app's real `src/main.ts` bootstraps `app-root` with the full
 * `app.routes.ts` graph and renders into the existing DOM. This alternate entry
 * exists ONLY for `nav.e2e.mjs`: it bootstraps the SAME app shell + real Angular
 * `provideRouter`, but EXPOSES the bootstrapped `Router` + `ApplicationRef` on
 * `globalThis` so the headless harness can DRIVE navigation
 * (`router.navigateByUrl('/metrics')`) across every route and assert the
 * `<router-outlet>` content changes per route — i.e. the nav links actually
 * work, the lazy route components load through the router, and each route view
 * renders.
 *
 * It builds against the FULL route graph — every authoring source the Rust
 * compiler now lowers to VALID, parseable Ivy (mirrors `app.routes.ts`):
 *
 *   - `''`        eager  → log-viewer.component.ts   (@Component .ts, the index)
 *   - `dashboard` lazy   → dashboard.component.ts     (@Component .ts)
 *   - `metrics`   lazy   → metrics-panel.component.ts (@Component .ts hosting the
 *                          `.treaty` Gauge + a pipe + a selectorless directive,
 *                          both now lowered to real Ivy `ɵɵdefinePipe` /
 *                          `ɵɵdefineDirective` defs)
 *   - `greeter`   lazy   → greeter-page.component.ts  (hosts the `.treaty` SFC
 *                          whose inline `server { … }` block is now extracted to a
 *                          typed binding, so the SFC lowers to valid client JS)
 *   - `profile`   lazy   → profile.routes.ts          (loadChildren nested routes)
 *
 * The greeter route was previously omitted because `greeter.treaty`'s inline
 * `server { … }` block was emitted VERBATIM (non-parseable JS — esbuild threw
 * `Unexpected "{"`); that extraction now lands in Rust, so the route is included
 * and `nav.e2e.mjs` drives the router across the WHOLE graph.
 */
import { ApplicationRef, provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'
import { provideRouter, Router, type Routes } from '@angular/router'

// Global BASE theme (mirrors the real `main.ts`): a plain `.css` side-effect
// import that Vite injects/emits, exercising the global stylesheet through the
// real `@treaty/vite` pipeline (which must not claim `.css`).
import './styles.css'

import { AppRoot } from './app/app-root.component'

// The FULL app route graph (mirrors app.routes.ts). Each lazy boundary is a real
// dynamic import the router loads — including the greeter route, now that its
// `.treaty` SFC's inline `server {}` block is extracted and the SFC lowers to
// valid client JS.
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
		path: 'metrics',
		loadComponent: () => import('./features/metrics/metrics-panel.component'),
	},
	{
		path: 'greeter',
		loadComponent: () => import('./features/greeter/greeter-page.component'),
	},
	{
		path: 'profile',
		loadChildren: () => import('./features/profile/profile.routes'),
	},
]

void bootstrapApplication(AppRoot, {
	providers: [provideZonelessChangeDetection(), provideRouter(routes)],
}).then((appRef) => {
	// Expose the bootstrapped Router + ApplicationRef so the headless harness can
	// drive navigation and force a synchronous render between hops. This is the
	// ONLY harness-specific seam; the routing itself is plain Angular.
	const router = appRef.injector.get(Router)
	const seam = globalThis as unknown as { treatyNav?: unknown; treatyNavReady?: boolean }
	seam.treatyNav = {
		router,
		appRef,
		tick: () => appRef.tick(),
		navigate: (url: string) => router.navigateByUrl(url),
	}
	seam.treatyNavReady = true
	void ApplicationRef // referenced for the type import above
})
