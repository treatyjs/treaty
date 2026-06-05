import { AsyncPipe } from '@angular/common';
import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { ProductService } from '../../core/product.service';
import { CurrencyFormatPipe } from '../../shared/currency-format.pipe';

@Component({
  selector: 'app-catalog',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AsyncPipe, RouterLink, CurrencyFormatPipe],
  template: `
    <section>
      <h2>Catalog</h2>
      @if (products$ | async; as products) {
        <ul class="grid">
          @for (product of products; track product.id) {
            <li class="panel">
              <a [routerLink]="['/catalog', product.id]">{{ product.name }}</a>
              <p class="muted">{{ product.category }}</p>
              <strong>{{ product.priceCents | currencyFormat }}</strong>
            </li>
          }
        </ul>
      } @else {
        <p>Loading…</p>
      }
    </section>
  `,
  styles: [
    `
      .grid {
        list-style: none;
        padding: 0;
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
        gap: 1rem;
      }
      .muted {
        color: var(--muted);
        text-transform: capitalize;
        margin: 0.25rem 0;
      }
    `,
  ],
})
export class Catalog {
  protected readonly products$ = inject(ProductService).list();
}
