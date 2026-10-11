import { Component, ChangeDetectionStrategy } from '@angular/core';

@Component({ selector: 'app-default-cd', template: '<p>d</p>', changeDetection: ChangeDetectionStrategy.Default })
export class DefaultCdComponent {}
