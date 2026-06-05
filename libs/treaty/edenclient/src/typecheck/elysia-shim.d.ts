/**
 * Minimal local shim for `elysia` used ONLY for standalone `tsc` typechecking of this library
 * in environments where the `elysia` peer package is not installed. The production build relies on
 * the real `elysia` types supplied by the consuming application. This file is never published.
 */
declare module 'elysia' {
  // The library only ever references `Elysia<...>` structurally via its `schema` member, so a
  // permissive generic declaration with a `schema` property is sufficient for type inference.
  export class Elysia<
    A = any,
    B = any,
    C = any,
    D = any,
    E = any,
    F = any
  > {
    schema: Record<string, any>;
  }
}
