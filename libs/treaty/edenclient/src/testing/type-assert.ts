/**
 * Tiny compile-time type-assertion utilities (no runtime cost) used by the typing specs.
 * If an assertion is wrong the file fails to typecheck under `tsc`.
 */

/** Exact-equality check between two types. */
export type Equal<X, Y> = (<T>() => T extends X ? 1 : 2) extends <
  T
>() => T extends Y ? 1 : 2
  ? true
  : false

/** Passes only when `T` is exactly `true`. */
export type Expect<T extends true> = T

/** Asserts `A` is assignable to `B`. */
export type Extends<A, B> = A extends B ? true : false
