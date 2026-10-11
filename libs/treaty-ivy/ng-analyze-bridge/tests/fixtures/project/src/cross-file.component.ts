import { Component, Input } from '@angular/core';
import { CARD_SELECTOR, CARD_INPUT_OPTIONS } from './shared/constants';

@Component({
  selector: CARD_SELECTOR,
  template: '<p>{{ heading }}</p>',
})
export class CrossFileComponent {
  @Input(CARD_INPUT_OPTIONS) heading = '';
}
