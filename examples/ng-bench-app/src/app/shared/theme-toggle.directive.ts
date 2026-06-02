import {
  Directive,
  HostBinding,
  HostListener,
  signal,
} from '@angular/core';

/**
 * Attribute directive demonstrating @HostBinding + @HostListener.
 * Toggles a `data-theme` attribute and an `is-dark` class on its host element.
 */
@Directive({
  selector: '[themeToggle]',
})
export class ThemeToggle {
  private readonly dark = signal(true);

  @HostBinding('attr.data-theme')
  get theme(): 'dark' | 'light' {
    return this.dark() ? 'dark' : 'light';
  }

  @HostBinding('class.is-dark')
  get isDark(): boolean {
    return this.dark();
  }

  @HostListener('click')
  onClick(): void {
    this.dark.update((value) => !value);
  }
}
