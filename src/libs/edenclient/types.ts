import type { Elysia } from 'elysia';
import { Observable } from 'rxjs';
import type { IsUnknown, UnionToIntersect } from './utils/typesafe.ts';

/**
 * Represents a list of files, typically obtained from an `<input type="file">` element.
 */
interface FileList {
  readonly length: number;
  item(index: number): File | null;
  [index: number]: File;
}

/**
 * Type representing a single File or a FileList, facilitating operations on file inputs.
 */
type Files = File | FileList;

/**
 * Replaces a target type within a record with a generic type, used for type transformations.
 */
type Replace<RecordType, TargetType, GenericType> = {
  [K in keyof RecordType]: RecordType[K] extends TargetType
    ? GenericType
    : RecordType[K];
};

/**
 * Splits a string by '/' and provides an array of segments, aiding in processing string-based paths.
 */
type Split<S extends string> = S extends `${infer Head}/${infer Tail}`
  ? Head extends ''
    ? Tail extends ''
      ? []
      : Split<Tail>
    : [Head, ...Split<Tail>]
  : S extends `/`
  ? []
  : S extends `${infer Head}/`
  ? [Head]
  : [S];

/**
* Defines a schema for API endpoints, specifying expected request and response structures.
*/
type AnySchema = {
  body: unknown;
  headers: unknown;
  query: unknown;
  params: unknown;
  response: any;
};

/**
 * Defines the client creation process, converting an Elysia application schema into a set of typed API endpoints.
 */
export namespace EdenClient {
  export type Create<App extends Elysia<any, any, any, any, any, any>> =
    App extends {
      schema: infer Schema extends Record<string, any>;
    }
      ? UnionToIntersect<Sign<Schema>>
      : 'Please install Elysia before using EdenClient';

  export type DetailedResponse<T> = {
    data: T;
    error: any;
    status: number;
    headers: Record<string, string>;
  };

  export type ObservableResponse<T> = Observable<DetailedResponse<T>>;

  export type Sign<
    Schema extends Record<string, Record<string, unknown>>,
    Paths extends (string | number)[] = Split<keyof Schema & string>,
    Carry extends string = ''
  > = Paths extends [
    infer Prefix extends string | number,
    ...infer Rest extends (string | number)[]
  ]
    ? {
        [Key in Prefix as Prefix extends `:${string}`
          ? (string & {}) | number | Prefix
          : Prefix]: Sign<Schema, Rest, `${Carry}/${Key}`>;
      }
    : Schema[Carry extends '' ? '/' : Carry] extends infer Routes
    ? {
        [Method in keyof Routes]: Routes[Method] extends infer Route extends AnySchema
          ? Method extends 'subscribe'
            ? never
            : (
                params?: (IsUnknown<Route['body']> extends false
                  ? Replace<Route['body'], Blob | Blob[], Files>
                  : {}) &
                  (undefined extends Route['query']
                    ? {
                        $query?: Record<string, string>;
                      }
                    : {
                        $query: Route['query'];
                      }) &
                  (undefined extends Route['headers']
                    ? {
                        $headers?: Record<string, unknown>;
                      }
                    : {
                        $headers: Route['headers'];
                      })
              ) => ObservableResponse<
                Route['response'] extends { 200: infer ReturnedType }
                  ? ReturnedType
                  : unknown
              >
          : never;
      }
    : {};
}
