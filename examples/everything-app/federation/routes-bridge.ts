/**
 * Routes bridge: the app's Angular `Routes` typed as Treaty's structural
 * `RouteLike[]`.
 *
 * `@treaty/module-federation` accepts the app's route graph as `RouteLike[]` —
 * a deliberately minimal structural subset of Angular's `Route` (just the
 * `path` + lazy-loader fields it needs to find federation boundaries). Angular's
 * `Route` interface carries no index signature, while `RouteLike` declares one
 * (`[extra: string]: unknown`) so it can ignore the rest of `Route`'s fields, so
 * a direct assignment trips TypeScript's index-signature check. The values are
 * fully compatible at run time — `deriveExposesFromRoutes` only ever reads
 * `path` / `loadComponent` / `loadChildren` / `children` — so we restate the
 * type once here, in one well-documented place, instead of casting at every
 * call site. Every MF config imports `appRoutes` from here.
 */
import type { RouteLike } from '@treaty/module-federation'

import { routes } from '../src/routes/app.routes'

/** The app's route graph, viewed through Treaty's structural `RouteLike` lens. */
export const appRoutes: readonly RouteLike[] = routes as unknown as readonly RouteLike[]
