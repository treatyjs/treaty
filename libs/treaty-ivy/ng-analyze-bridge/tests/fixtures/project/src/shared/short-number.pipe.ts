import { Pipe, PipeTransform } from '@angular/core';

@Pipe({ name: 'shortNum', pure: false, standalone: true })
export class ShortNumberPipe implements PipeTransform {
  transform(value: number): string {
    return value >= 1000 ? `${Math.round(value / 1000)}k` : String(value);
  }
}
