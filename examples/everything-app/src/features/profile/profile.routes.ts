/**
 * Lazy feature: Profile, exposed as a child ROUTES file (loaded via
 * `loadChildren`). This makes the whole Profile feature a single federated
 * remote with its own nested routing.
 */
import type { Routes } from '@angular/router'
import { ProfileComponent } from './profile.component'
import { ProfileSettingsComponent } from './profile-settings.component'

const routes: Routes = [
	{ path: '', component: ProfileComponent },
	{ path: 'settings', component: ProfileSettingsComponent },
]

export default routes
