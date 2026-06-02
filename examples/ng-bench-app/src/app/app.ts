import { ChangeDetectionStrategy, Component, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';

import { ThemeToggleDirective } from './shared/theme-toggle.directive';

@Component({
  selector: 'app-root',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive, ThemeToggleDirective],
  template: `
    <header class="container">
      <h1>{{ title() }}</h1>
      <nav>
        <a routerLink="/" routerLinkActive="active" [routerLinkActiveOptions]="{ exact: true }">
          Dashboard
        </a>
        <a routerLink="/catalog" routerLinkActive="active">Catalog</a>
        <a routerLink="/about" routerLinkActive="active">About</a>
        <button type="button" class="secondary" appThemeToggle>Toggle theme</button>
      </nav>
    </header>

    <main class="container">
      <router-outlet />
    </main>
  `,
  styles: [
    `
      header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 1rem;
        flex-wrap: wrap;
      }
      nav {
        display: flex;
        gap: 1rem;
        align-items: center;
      }
      nav a.active {
        font-weight: 700;
        border-bottom: 2px solid var(--accent-2);
      }
    `,
  ],
})
export class App {
  protected readonly title = signal('Ng Bench App');
}
