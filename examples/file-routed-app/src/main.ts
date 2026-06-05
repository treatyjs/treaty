/**
 * Browser bootstrap for the file-routed-app.
 *
 * Plain `.ts` (no `@Component`), so the `@treaty/vite` plugin passes it through
 * to Vite untouched — only the route authoring files it pulls in transitively
 * (`.treaty` / `.tjsx`) are lowered to Ivy. It wires the standalone application:
 *
 *   - `provideRouter(routes)` consumes the route graph produced DURING the build
 *     as the `virtual:treaty-routes` virtual module: `@treaty/vite`'s `fileRoutes`
 *     option lowers the on-disk `routes/` directory tree to an Angular route graph
 *     via the `treaty_file_routing` Rust core on every load — there is no
 *     checked-in / prebuilt `routes.ts`. The route components are all lazy
 *     `loadComponent` boundaries.
 *   - `provideZonelessChangeDetection()` because the route components are
 *     signal-by-default and OnPush throughout (no zone.js).
 *
 * Treaty is a compiler, not a host: this is a standard standalone Angular
 * bootstrap. Treaty's contribution is the file-system route lowering plus having
 * compiled every `.treaty` / `.tjsx` route component to Ivy at build time.
 */
import { provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'
import { provideRouter } from '@angular/router'

import { RoutedRoot } from './app/routed-root.component'
import { routes } from 'virtual:treaty-routes'

void bootstrapApplication(RoutedRoot, {
	providers: [provideZonelessChangeDetection(), provideRouter(routes)],
})
