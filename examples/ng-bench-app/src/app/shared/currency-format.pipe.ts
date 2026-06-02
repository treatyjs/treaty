import { Pipe, PipeTransform } from '@angular/core';

/**
 * Standalone pure pipe implementing PipeTransform.
 * Formats a number of cents into a localized currency string.
 */
@Pipe({
  name: 'currencyFormat',
})
export class CurrencyFormatPipe implements PipeTransform {
  transform(cents: number, currency = 'USD', locale = 'en-US'): string {
    const amount = (cents ?? 0) / 100;
    return new Intl.NumberFormat(locale, {
      style: 'currency',
      currency,
    }).format(amount);
  }
}
