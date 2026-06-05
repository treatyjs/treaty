import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input,
} from '@angular/core';
import { toObservable, toSignal } from '@angular/core/rxjs-interop';
import { RouterLink } from '@angular/router';
import { switchMap } from 'rxjs/operators';

import { Product } from '../../core/product.model';
import { ProductService } from '../../core/product.service';
import { CurrencyFormatPipe } from '../../shared/currency-format.pipe';

@Component({
  selector: 'app-product-detail',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, CurrencyFormatPipe],
  template: `
    <section>
      <p><a routerLink="/catalog">← Back to catalog</a></p>
      @if (product(); as p) {
        <article class="panel">
          <h2>{{ p.name }}</h2>
          <dl>
            <dt>Category</dt>
            <dd>{{ p.category }}</dd>
            <dt>Price</dt>
            <dd>{{ p.priceCents | currencyFormat }}</dd>
            <dt>Availability</dt>
            <dd>{{ p.inStock ? 'In stock' : 'Backorder' }}</dd>
          </dl>
        </article>
      } @else {
        <p>Product #{{ id() }} not found.</p>
      }
    </section>
  `,
})
export class ProductDetail {
  /**
   * Bound from the `:id` route parameter via withComponentInputBinding().
   * The string transform keeps it a number for the service lookup.
   */
  readonly id = input.required<number, string>({
    transform: (raw: string) => Number(raw),
  });

  private readonly service = inject(ProductService);

  /** id signal -> observable -> service lookup -> signal, all reactive. */
  protected readonly product = toSignal<Product | undefined>(
    toObservable(this.id).pipe(switchMap((id) => this.service.byId(id))),
    { initialValue: undefined },
  );
}
