/**
 * Browser bootstrap for the file-routed-app.
 *
 * Plain `.ts` (no `@Component`), so the `@treaty/vite` plugin passes it through
 * to Vite untouched — only the route authoring files it pulls in transitively
 * (`.treaty` / `.tjsx`) are lowered to Ivy. It wires the standalone application:
 *
 *   - `provideRouter(routes)` consumes the GENERATED route graph
 *     (`src/generated/routes.ts`), which the build-time route generator lowers
 *     from the `routes/` directory tree using the `treaty_file_routing`
 *     convention. The route components are all lazy `loadComponent` boundaries.
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
import { routes } from './generated/routes'

void bootstrapApplication(RoutedRoot, {
	providers: [provideZonelessChangeDetection(), provideRouter(routes)],
})
