/**
 * Browser bootstrap for the everything-app.
 *
 * This is a plain `.ts` module (no `@Component`), so the `@treaty/vite` plugin
 * passes it straight through to Vite — only the authoring files it imports
 * transitively (`.treaty` / `.tsx` / `.tjsx` / `@Component` `.ts`) are lowered
 * to Ivy. It wires the standalone application:
 *
 *   - `provideRouter(routes)` consumes the app's route graph (`app.routes.ts`):
 *     the eager index plus the four lazy feature routes, whose `loadComponent` /
 *     `loadChildren` boundaries are exactly what Treaty's `deriveExposesFromRoutes`
 *     turns into Module Federation remotes.
 *   - `provideZonelessChangeDetection()` because the app is signal-by-default and
 *     OnPush throughout — there is no zone.js dependency to pull in.
 *
 * Treaty is a compiler, not a host: nothing here is Treaty-specific. The app is a
 * standard standalone Angular bootstrap; Treaty's only contribution is having
 * lowered every authoring file in the graph to Ivy at build time.
 */
import { provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'
import { provideRouter } from '@angular/router'

import { AppRoot } from './app/app-root.component'
import { routes } from './routes/app.routes'

void bootstrapApplication(AppRoot, {
	providers: [provideZonelessChangeDetection(), provideRouter(routes)],
})
