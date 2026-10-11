import { Directive, input } from '@angular/core';

@Directive({ selector: '[appBadge]', standalone: true, exportAs: 'badge' })
export class BadgeDirective {
  tone = input<string>('neutral');
}
