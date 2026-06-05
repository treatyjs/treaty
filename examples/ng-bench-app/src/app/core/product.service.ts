import { Injectable } from '@angular/core';
import { Observable, of } from 'rxjs';
import { delay } from 'rxjs/operators';

import { LoggerService } from './logger.service';
import { Product } from './product.model';

const SEED: readonly Product[] = [
  { id: 1, name: 'Ivy Linker Pro', category: 'software', priceCents: 4900, inStock: true },
  { id: 2, name: 'OXC Parser Core', category: 'software', priceCents: 0, inStock: true },
  { id: 3, name: 'Treaty Build Server', category: 'service', priceCents: 19900, inStock: true },
  { id: 4, name: 'Compile Cluster Node', category: 'hardware', priceCents: 129900, inStock: false },
  { id: 5, name: 'Signals Devkit', category: 'software', priceCents: 2900, inStock: true },
  { id: 6, name: 'Zoneless Support Plan', category: 'service', priceCents: 9900, inStock: true },
];

/**
 * Root-provided data service with explicit constructor DI (LoggerService).
 * Returns RxJS observables to exercise the async pipe / reactive code paths.
 */
@Injectable({ providedIn: 'root' })
export class ProductService {
  constructor(private readonly logger: LoggerService) {}

  list(): Observable<readonly Product[]> {
    this.logger.log('ProductService.list()');
    return of(SEED).pipe(delay(0));
  }

  byId(id: number): Observable<Product | undefined> {
    this.logger.log(`ProductService.byId(${id})`);
    return of(SEED.find((p) => p.id === id)).pipe(delay(0));
  }
}
