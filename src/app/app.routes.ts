import { Routes } from '@angular/router';

export const routes: Routes = [
  {
    path: '',
    redirectTo: 'post/1',
    pathMatch: 'full',
  },
  {
    path: 'post/:id',
    loadComponent: () => import('./post/post.component'),
  },
];
