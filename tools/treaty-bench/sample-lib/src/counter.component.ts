import { Component } from '@angular/core';

@Component({
  selector: 'tb-counter',
  standalone: true,
  template: '<button (click)="inc()">count {{ count }}</button>',
})
export class CounterComponent {
  count: number = 0;
  inc(): void {
    this.count++;
  }
}
