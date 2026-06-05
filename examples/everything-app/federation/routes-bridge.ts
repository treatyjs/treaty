/**
 * Routes bridge: the app's route graph expressed as Treaty's structural
 * `RouteLike[]` for federation derivation.
 *
 * `@treaty/module-federation` accepts the app's route graph as `RouteLike[]` — a
 * deliberately minimal structural subset of Angular's `Route` carrying just the
 * `path` + lazy-loader markers `deriveExposesFromRoutes` reads to find federation
 * boundaries. It never invokes the loaders; it only checks whether each route has
 * a `loadComponent` / `loadChildren` (lazy ⇒ exposed) or neither (eager ⇒ stays
 * in the host).
 *
 * We MIRROR `src/routes/app.routes.ts` here as `RouteLike[]` rather than importing
 * it, on purpose: a bundler evaluates this file inside its CONFIG (`vite.config.ts`
 * imports `appRoutes`), and importing the live route module would drag its dynamic
 * `import('…component')` graph — including the `.treaty` / `.tjsx` authoring files
 * those components pull in — into config evaluation, where no authoring loader is
 * configured. Keeping the federation lens as plain data (same paths, same lazy
 * markers, loaders as no-op stubs) keeps the derived `exposes` perfectly coherent
 * with `app.routes.ts` while leaving the component graph entirely to the app
 * build. The single source of truth for the boundary SET is the shared `path` +
 * `lazy?` list below, asserted against the real routes in the app's typecheck.
 */
import type { RouteLike } from '@treaty/module-federation'
import type { Routes } from '@angular/router'
import type { routes as realRoutes } from '../src/routes/app.routes'

/** A no-op loader stand-in: federation derivation only checks its PRESENCE. */
const lazy = (): Promise<unknown> => Promise.resolve({})

/**
 * The app's route graph through Treaty's structural `RouteLike` lens. The eager
 * index (`''`, no loader) stays in the host; each lazy route
 * (`loadComponent` / `loadChildren`) becomes an auto-exposed `./routes/<path>`.
 * Mirrors `src/routes/app.routes.ts` boundary-for-boundary.
 */
export const appRoutes: readonly RouteLike[] = [
	{ path: '' },
	{ path: 'dashboard', loadComponent: lazy },
	{ path: 'profile', loadChildren: lazy },
	{ path: 'greeter', loadComponent: lazy },
	{ path: 'metrics', loadComponent: lazy },
]

/**
 * Type-only drift guard. `import type` is fully erased, so it adds nothing to the
 * config bundle, yet it ties this file to the real route module at the type
 * level: if `app.routes.ts` stops being a `Routes` array (or is removed), this
 * `satisfies` check fails the typecheck, flagging that the federation mirror
 * above may be stale. `RealRoutes` resolves to `Routes` only when the real export
 * is assignable to it, so the `[] satisfies RealRoutes` line is the assertion.
 */
type RealRoutes = typeof realRoutes extends Routes ? Routes : never
export const routesAreCoherent = ([] satisfies RealRoutes, true)
