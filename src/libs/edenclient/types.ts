/// <reference lib="dom" />
import type { Elysia } from 'elysia';
import { Observable } from 'rxjs';
import type { IsUnknown, UnionToIntersect } from './utils/typesafe';

type Files = File | FileList;

type Replace<RecordType, TargetType, GenericType> = {
  [K in keyof RecordType]: RecordType[K] extends TargetType
    ? GenericType
    : RecordType[K];
};

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

type AnySchema = {
  body: unknown;
  headers: unknown;
  query: unknown;
  params: unknown;
  response: any;
};

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
