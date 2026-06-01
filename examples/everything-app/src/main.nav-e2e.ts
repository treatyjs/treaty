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
 * It builds against the subset of routes whose authoring sources the Rust
 * compiler currently lowers to VALID, parseable Ivy (mirrors `main.full-e2e.ts`):
 *
 *   - `''`        eager  → log-viewer.component.ts   (@Component .ts, the index)
 *   - `dashboard` lazy   → dashboard.component.ts     (@Component .ts)
 *   - `metrics`   lazy   → metrics-panel.component.ts (@Component .ts hosting the
 *                          `.treaty` Gauge + a pipe + a selectorless directive)
 *   - `profile`   lazy   → profile.routes.ts          (loadChildren nested routes)
 *
 * The `greeter` route is DELIBERATELY omitted here because its `.treaty` SFC
 * (`greeter.treaty`) hits a Rust-compiler lowering gap: its inline
 * `server { … }` server-fn block is emitted VERBATIM into the lowered module
 * instead of being extracted, producing non-parseable JS (`server { async
 * function … }` → esbuild type-strip throws `Unexpected "{"`), so any bundler —
 * dev-serve transform or `vite build` — fails to lower it. `nav.e2e.mjs`
 * REPORTS that Rust gap precisely (it is owned by `libs/treaty-ivy` /
 * `libs/authoring/node`, out of scope to edit) and proves the reported dev-serve
 * MIME fix for `.treaty` separately over the real HTTP server, while this entry
 * drives the router across every route that lowers cleanly.
 */
import { ApplicationRef, provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'
import { provideRouter, Router, type Routes } from '@angular/router'

// Global BASE theme (mirrors the real `main.ts`): a plain `.css` side-effect
// import that Vite injects/emits, exercising the global stylesheet through the
// real `@treaty/vite` pipeline (which must not claim `.css`).
import './styles.css'

import { AppRoot } from './app/app-root.component'

// The working subset of the app's routes (mirrors app.routes.ts minus the
// greeter route, whose .treaty SFC is blocked by the reported Rust `server {}`
// extraction gap). Each lazy boundary is a real dynamic import the router loads.
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
