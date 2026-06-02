import { Routes } from '@angular/router';

import { Dashboard } from './features/dashboard/dashboard';

/**
 * Eager dashboard at the root plus two lazy-loaded feature routes, so the
 * benchmark exercises code-splitting / dynamic-import boundaries the same way
 * a real app does.
 */
export const routes: Routes = [
  {
    path: '',
    pathMatch: 'full',
    component: Dashboard,
    title: 'Dashboard',
  },
  {
    path: 'catalog',
    title: 'Catalog',
    loadComponent: () =>
      import('./features/catalog/catalog').then((m) => m.Catalog),
  },
  {
    path: 'catalog/:id',
    title: 'Product detail',
    loadComponent: () =>
      import('./features/catalog/product-detail').then((m) => m.ProductDetail),
  },
  {
    path: 'about',
    title: 'About',
    loadComponent: () => import('./features/about/about').then((m) => m.About),
  },
  {
    path: '**',
    redirectTo: '',
  },
];
