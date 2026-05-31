/**
 * Application routes.
 *
 * The two LAZY routes below -- one `loadComponent`, one `loadChildren` -- are
 * the federation seam: Treaty's `deriveExposesFromRoutes` walks this array and
 * turns every lazy boundary into an independently deployable Module Federation
 * remote with NO hand-written `exposes` map. The eager `''` route stays in the
 * host.
 */
import type { Routes } from '@angular/router'

export const routes: Routes = [
	{
		// Eager index route -- stays in the host bundle.
		path: '',
		loadComponent: () => import('../components/log-viewer.component').then((m) => m.LogViewer),
	},
	{
		// Lazy single-component remote (auto-exposed as ./routes/dashboard).
		path: 'dashboard',
		loadComponent: () => import('../features/dashboard/dashboard.component'),
	},
	{
		// Lazy child-routes remote (auto-exposed as ./routes/profile).
		path: 'profile',
		loadChildren: () => import('../features/profile/profile.routes'),
	},
	{
		// Lazy single-component remote (auto-exposed as ./routes/greeter). Its
		// component hosts the .treaty + .tjsx surfaces and calls an extracted
		// server fn -- a lazy boundary that becomes a federated remote.
		path: 'greeter',
		loadComponent: () => import('../features/greeter/greeter-page.component'),
	},
]

export default routes
