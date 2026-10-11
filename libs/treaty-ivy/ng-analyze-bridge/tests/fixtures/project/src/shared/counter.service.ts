import { Injectable, Optional, Inject, InjectionToken } from '@angular/core';

export const COUNTER_START = new InjectionToken<number>('counter.start');

@Injectable({ providedIn: 'root' })
export class CounterService {
  constructor(@Optional() @Inject(COUNTER_START) private start: number | null, private other: LegacyDep) {}
  next() { return (this.start ?? 0) + 1; }
}

export class LegacyDep {}
