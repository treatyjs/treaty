import { Component, input } from '@angular/core';
import { RouterOutlet } from '@angular/router';
import TreatExample from './treat-example.treaty';

@Component({
  selector: 'app-root',
  standalone: true,
  imports: [RouterOutlet, TreatExample],
  template: `<h1>working</h1> <treat-example [val]="val()"></treat-example>`,
})
export class AppComponent {
  val = input('test');
}
