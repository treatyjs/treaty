import { Component, HostBinding, computed, input, output, model, viewChild, ElementRef, ChangeDetectionStrategy } from '@angular/core';
import { ShortNumberPipe } from './shared/short-number.pipe';

@Component({
  selector: 'app-stat-card',
  standalone: true,
  imports: [ShortNumberPipe],
  template: `
    <section class="card">
      <h3>{{ label() }}</h3>
      <span #valueEl>{{ value() | shortNum }}</span>
      <button (click)="reset.emit()">Reset</button>
    </section>
  `,
  styleUrl: './stat-card.component.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    'role': 'region',
    '[class.active]': 'isActive()',
    '(keydown.enter)': 'onEnter($event)',
  },
  exportAs: 'statCard',
})
export class StatCardComponent {
  label = input.required<string>();
  value = input(0, { alias: 'count' });
  reset = output<void>();
  selection = model<number>(0);
  valueEl = viewChild.required<ElementRef<HTMLSpanElement>>('valueEl');

  @HostBinding('attr.data-kind') kind = 'stat';

  isActive() { return this.value() > 0; }
  onEnter(e: KeyboardEvent) { return e; }
}
