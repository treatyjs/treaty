import { Routes } from '@angular/router';
import { resolvePost } from './post/post.component.ts';

export const routes: Routes = [
  {
    path: '',
    redirectTo: 'post/1',
    pathMatch: 'full',
  },
  {
    path: 'post/:id',
    loadComponent: () => import('./post/post.component.ts'),
    resolve: {
      ...resolvePost,
    },
  },
];
