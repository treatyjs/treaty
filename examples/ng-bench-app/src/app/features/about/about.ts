import { ChangeDetectionStrategy, Component, signal } from '@angular/core';

@Component({
  selector: 'app-about',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="panel">
      <h2>About this fixture</h2>
      <p>
        This is a standard, pure-Angular application used as a fair, apples-to-apples
        compile benchmark. It builds with both the Angular CLI
        (<code>@angular/build:application</code>) and Treaty's bundler plugins because it
        uses zero Treaty-only features.
      </p>
      <p>Counter (demonstrates a signal + event binding): {{ count() }}</p>
      <button type="button" (click)="increment()">Increment</button>
    </section>
  `,
})
export class About {
  protected readonly count = signal(0);

  protected increment(): void {
    this.count.update((n) => n + 1);
  }
}
