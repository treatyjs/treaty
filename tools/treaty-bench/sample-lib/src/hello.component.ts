import { Component, Input } from '@angular/core';

@Component({
  selector: 'tb-hello',
  standalone: true,
  template: '<h1 class="greeting">Hello {{ name }}</h1>',
})
export class HelloComponent {
  @Input() name: string = 'World';
}
