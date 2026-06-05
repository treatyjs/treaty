import { Injectable } from '@angular/core';

/**
 * Trivial root-provided service used purely to demonstrate constructor
 * dependency injection from another service.
 */
@Injectable({ providedIn: 'root' })
export class LoggerService {
  private readonly entries: string[] = [];

  log(message: string): void {
    const line = `[${new Date().toISOString()}] ${message}`;
    this.entries.push(line);
    // eslint-disable-next-line no-console
    console.debug(line);
  }

  history(): readonly string[] {
    return this.entries;
  }
}
