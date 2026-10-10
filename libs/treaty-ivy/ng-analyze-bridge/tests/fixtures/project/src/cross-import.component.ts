import { Component } from '@angular/core';
import { BadgeDirective } from './shared/badge.directive';

@Component({
  selector: 'app-cross-import',
  imports: [BadgeDirective],
  template: '<span appBadge>x</span>',
})
export class CrossImportComponent {}
