import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';

import { LoggerService } from '../../core/logger.service';
import { Product } from '../../core/product.model';
import { ProductService } from '../../core/product.service';
import { CurrencyFormatPipe } from '../../shared/currency-format.pipe';
import { StatCard, Trend } from '../../shared/stat-card';

@Component({
  selector: 'app-dashboard',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [StatCard, CurrencyFormatPipe],
  template: `
    <section>
      <h2>Inventory dashboard</h2>

      <div class="cards">
        <app-stat-card
          label="Products"
          [value]="products().length"
          trend="up"
          (refresh)="onRefresh($event)"
        />
        <app-stat-card
          label="In stock"
          [value]="inStockCount()"
          [trend]="stockTrend()"
          (refresh)="onRefresh($event)"
        />
        <app-stat-card
          label="Catalogue value (cents)"
          [value]="totalValueCents()"
          trend="flat"
          (refresh)="onRefresh($event)"
        />
      </div>

      <div class="panel">
        <label>
          Filter by category:
          <select [value]="category()" (change)="setCategory($any($event.target).value)">
            <option value="all">All</option>
            <option value="hardware">Hardware</option>
            <option value="software">Software</option>
            <option value="service">Service</option>
          </select>
        </label>
      </div>

      @if (visible().length > 0) {
        <ul class="list">
          @for (product of visible(); track product.id) {
            <li>
              <span class="name">{{ product.name }}</span>
              <span class="price">{{ product.priceCents | currencyFormat }}</span>
              @if (!product.inStock) {
                <span class="badge">backorder</span>
              }
            </li>
          } @empty {
            <li>No products match.</li>
          }
        </ul>
      } @else {
        <p>Loading products…</p>
      }

      <p class="muted">Total catalogue value: {{ totalValueCents() | currencyFormat }}</p>
    </section>
  `,
  styles: [
    `
      .cards {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
        gap: 1rem;
      }
      .list {
        list-style: none;
        padding: 0;
      }
      .list li {
        display: flex;
        gap: 1rem;
        align-items: center;
        padding: 0.5rem 0;
        border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      }
      .name {
        flex: 1;
      }
      .badge {
        font-size: 0.75rem;
        color: var(--danger);
        border: 1px solid var(--danger);
        border-radius: 999px;
        padding: 0 0.5rem;
      }
      .muted {
        color: var(--muted);
      }
    `,
  ],
})
export class Dashboard {
  private readonly products$ = inject(ProductService).list();
  private readonly logger = inject(LoggerService);

  /** Stream -> signal. Empty array until the first emission. */
  protected readonly products = toSignal(this.products$, {
    initialValue: [] as readonly Product[],
  });

  protected readonly category = signal<'all' | Product['category']>('all');

  protected readonly visible = computed(() => {
    const cat = this.category();
    const all = this.products();
    return cat === 'all' ? all : all.filter((p) => p.category === cat);
  });

  protected readonly inStockCount = computed(
    () => this.products().filter((p) => p.inStock).length,
  );

  protected readonly totalValueCents = computed(() =>
    this.products().reduce((sum, p) => sum + p.priceCents, 0),
  );

  protected readonly stockTrend = computed<Trend>(() => {
    const total = this.products().length;
    if (total === 0) {
      return 'flat';
    }
    return this.inStockCount() / total >= 0.5 ? 'up' : 'down';
  });

  constructor() {
    // effect() reacting to signal changes (no-op side effect for the bench).
    effect(() => {
      this.logger.log(
        `Dashboard: ${this.visible().length} visible / ${this.products().length} total`,
      );
    });
  }

  protected setCategory(value: string): void {
    this.category.set(value as 'all' | Product['category']);
  }

  protected onRefresh(label: string): void {
    this.logger.log(`Refresh requested for "${label}"`);
  }
}
