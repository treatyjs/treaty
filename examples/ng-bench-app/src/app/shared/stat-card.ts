import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from '@angular/core';

export type Trend = 'up' | 'down' | 'flat';

/**
 * Presentational card driven entirely by signal inputs / outputs.
 * Demonstrates input() (incl. required + transform), output(), @switch and
 * a computed() derived value.
 */
@Component({
  selector: 'app-stat-card',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <article class="panel">
      <header>
        <h3>{{ label() }}</h3>
        <span class="trend">
          @switch (trend()) {
            @case ('up') {
              <span class="up">▲</span>
            }
            @case ('down') {
              <span class="down">▼</span>
            }
            @default {
              <span class="flat">—</span>
            }
          }
        </span>
      </header>
      <p class="value">{{ display() }}</p>
      <button type="button" (click)="refresh.emit(label())">Refresh</button>
    </article>
  `,
  styles: [
    `
      header {
        display: flex;
        justify-content: space-between;
        align-items: center;
      }
      .value {
        font-size: 2rem;
        margin: 0.25rem 0 0.75rem;
      }
      .up {
        color: var(--accent-2);
      }
      .down {
        color: var(--danger);
      }
      .flat {
        color: var(--muted);
      }
    `,
  ],
})
export class StatCard {
  /** Required signal input. */
  readonly label = input.required<string>();

  /** Optional signal input with a default. */
  readonly value = input<number>(0);

  /** Signal input with a transform function. */
  readonly trend = input<Trend, string>('flat', {
    transform: (raw: string): Trend =>
      raw === 'up' || raw === 'down' ? raw : 'flat',
  });

  /** Event emitted to the parent when the user clicks refresh. */
  readonly refresh = output<string>();

  protected readonly display = computed(() =>
    new Intl.NumberFormat('en-US').format(this.value()),
  );
}
