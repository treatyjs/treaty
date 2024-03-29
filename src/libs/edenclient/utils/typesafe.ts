/// Source https://github.com/elysiajs/eden/blob/main/src/types.ts
import type { EdenFetchError } from '../error.ts'

/**
 * Represents a type utility to create a range of numbers between two numeric types.
 * This is useful for generating types that represent a range of HTTP status codes, among other ranges.
 * https://stackoverflow.com/a/39495173
 * @template F The first number in the range.
 * @template T The last number in the range.
 */
type Range<F extends number, T extends number> = Exclude<
    Enumerate<T>,
    Enumerate<F>
>

/**
 * Generates a union of numbers up to a given number, facilitating the creation of numeric ranges.
 * This utility is foundational for defining ranges of numbers, such as HTTP status codes.
 * @template N The target number up to which the enumeration should occur.
 * @template Acc An accumulator array used internally for recursion.
 */
type Enumerate<
    N extends number,
    Acc extends number[] = []
> = Acc['length'] extends N
    ? Acc[number]
    : Enumerate<N, [...Acc, Acc['length']]>

/**
* Represents the range of HTTP error status codes, from 300 to 599.
* This type is primarily used for error handling and categorization.
*/
type ErrorRange = Range<300, 599>

/**
 * Maps HTTP error status codes to corresponding EdenFetchError types, providing a way to handle API errors in a type-safe manner.
 * @template T A record type where keys are HTTP status codes and values are error payloads.
 */
export type MapError<T extends Record<number, unknown>> = [
    {
        [K in keyof T]-?: K extends ErrorRange ? K : never
    }[keyof T]
] extends [infer A extends number]
    ? {
          [K in A]: EdenFetchError<K, T[K]>
      }[A]
    : false

/**
* Converts a union type to an intersection type. This is useful for combining multiple types into a single type where all the properties must exist.
* @template U The union type to be converted.
*/
export type UnionToIntersect<U> = (
    U extends any ? (arg: U) => any : never
) extends (arg: infer I) => void
    ? I
    : never

/**
* Converts a union type to a tuple type. This is useful for type-safe handling of union types where each member of the union needs to be considered separately.
* @template T The union type to be converted into a tuple.
*/
export type UnionToTuple<T> = UnionToIntersect<
    T extends any ? (t: T) => T : never
> extends (_: any) => infer W
    ? [...UnionToTuple<Exclude<T, W>>, W]
    : []

/**
* Determines if a type is `any`. This utility is helpful for type guards in TypeScript, ensuring that certain type manipulations are not performed on the `any` type.
* @template T The type to check against `any`.
*/
export type IsAny<T> = 0 extends 1 & T ? true : false

/**
 * Determines if a type is `never`. This is useful for validating exhaustive conditions in TypeScript, ensuring that all possible cases in union types are handled.
 * @template T The type to check against `never`.
 */
export type IsNever<T> = [T] extends [never] ? true : false

/**
 * Determines if a type is `unknown`. This utility aids in type-checking where `unknown` types require explicit handling or casting.
 * @template T The type to check against `unknown`.
 */
export type IsUnknown<T> = IsAny<T> extends true
    ? false
    : unknown extends T
    ? true
    : false

/**
* Represents the structure of a typed route within the EdenClient framework. This includes definitions for request body, headers, query parameters, path parameters, and response types.
* This type is crucial for defining the API contract in a type-safe manner.
*/
export type AnyTypedRoute = {
    body: unknown
    headers: Record<string, any> | undefined
    query: Record<string, any> | undefined
    params: Record<string, any> | undefined
    response: Record<string, unknown> & {
        '200': unknown
    }
}