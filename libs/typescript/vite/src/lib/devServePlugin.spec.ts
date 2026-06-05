import type { Plugin } from 'vite';
import { COMPILER_SCRIPT_RE, DevServePlugin } from './devServePlugin';

/**
 * The dev-serve HTML transform owner for `linkAngularPartials()`. Its whole reason to exist is the
 * "no JIT / no `@angular/compiler`" guarantee in dev: it must inject NO compiler script, and it must
 * strip one if some other plugin injected it. These tests pin both.
 */

/** Invoke the plugin's `transformIndexHtml` handler regardless of object/function form. */
function runIndexTransform(plugin: Plugin, html: string): string {
  const hook = plugin.transformIndexHtml;
  const handler =
    typeof hook === 'function'
      ? hook
      : hook && typeof hook === 'object'
        ? hook.handler
        : undefined;
  if (typeof handler !== 'function') {
    throw new Error('plugin has no transformIndexHtml handler');
  }
  // The Vite ctx arg is unused by this transform.
  const out = (handler as (h: string, ctx?: unknown) => string | undefined)(html);
  return out ?? html;
}

const HTML_NO_COMPILER =
  '<!doctype html><html><head><title>app</title></head><body><app-root></app-root></body></html>';

describe('DevServePlugin', () => {
  it('applies only on serve (dev), never on build', () => {
    expect(DevServePlugin.apply).toBe('serve');
  });

  it('injects NO <script src="/@angular/compiler"> into the dev HTML', () => {
    const out = runIndexTransform(DevServePlugin, HTML_NO_COMPILER);
    expect(out).not.toMatch(/@angular\/compiler/);
    expect(out).not.toMatch(/<script[^>]*compiler/i);
  });

  it('leaves a compiler-free document otherwise byte-identical (no extra rewriting)', () => {
    const out = runIndexTransform(DevServePlugin, HTML_NO_COMPILER);
    expect(out).toBe(HTML_NO_COMPILER);
  });

  it('strips a compiler script another plugin may have injected (defensive)', () => {
    const injected = HTML_NO_COMPILER.replace(
      '</head>',
      '<script type="module" src="/@angular/compiler"></script></head>',
    );
    const out = runIndexTransform(DevServePlugin, injected);
    expect(out).not.toMatch(/@angular\/compiler/);
    // The rest of the head is preserved.
    expect(out).toContain('<title>app</title>');
  });

  it('COMPILER_SCRIPT_RE matches both bare and /-prefixed compiler script srcs', () => {
    expect('<script src="@angular/compiler"></script>').toMatch(
      new RegExp(COMPILER_SCRIPT_RE.source, 'i'),
    );
    expect('<script type="module" src="/@angular/compiler"></script>').toMatch(
      new RegExp(COMPILER_SCRIPT_RE.source, 'i'),
    );
  });
});
