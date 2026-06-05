# EdenClient for Angular using Elysia Framework

EdenClient is a TypeScript-based client designed to interact seamlessly with APIs built using the Elysia framework. It leverages RxJS for observables and integrates with Angular's HttpClient for making HTTP requests, providing a type-safe way to communicate with your backend services.

## Features

- **Type-Safe API Calls**: Generate type-safe API clients automatically from your Elysia schema.
- **File Handling**: Simplified file uploads with built-in support for `File` and `FileList`.
- **Error Handling**: Advanced error handling capabilities, mapping HTTP error statuses to `EdenFetchError` instances.
- **Observable Responses**: Utilizes RxJS observables for handling asynchronous data and error flows.



## Installation

EdenClient can be installed using various package managers. Choose the one that matches your project's environment:

### Deno

```sh
import * as mod from "jsr:@treaty/httpclient@0.0";
```

### NPM

```sh
npx jsr i @treaty/httpclient
```

```javascript
import * as mod from "@treaty/httpclient";
```

### Yarn

```sh
yarn dlx jsr i @treaty/httpclient
```

```javascript
import * as mod from "@treaty/httpclient";
```

### pnpm

```sh
pnpm dlx jsr i @treaty/httpclient
```

```javascript
import * as mod from "@treaty/httpclient";
```

### Bun

```sh
bunx jsr i @treaty/httpclient
```

```javascript
import * as mod from "@treaty/httpclient";
```

## Basic Usage

To start using EdenClient with your Elysia-based API, you first need to create an instance of the client by specifying the base URL of your API:

```typescript
import { edenClient } from '@treaty/httpclient';

const client = edenClient<App>('http://localhost:5555').api;
```

Replace `App` with your application's specific type that describes your API schema.


## Entry points

This package exposes three entry points:

| Import                            | Surface                                                                 |
| --------------------------------- | ----------------------------------------------------------------------- |
| `@treaty/httpclient`              | Barrel — re-exports everything below (plus the legacy `edenClient`).     |
| `@treaty/httpclient/client`       | The typed HTTP client: `createClient`, promise helpers, `EdenClient`.   |
| `@treaty/httpclient/resources`    | Angular signal resources: `edenResource` / `edenHttpResource`.          |

### `client` — typed HTTP client

```typescript
import { createClient } from '@treaty/httpclient/client';

const client = createClient<App>('http://localhost:3000');

// Observable form (back-compatible with edenClient):
client.users.get().subscribe((res) => console.log(res.data));

// Promise form:
import { createPromiseClient, asPromiseClient } from '@treaty/httpclient/client';
const pclient = createPromiseClient<App>('http://localhost:3000');
const res = await pclient.users.get(); // typed DetailedResponse
```

`createClient<App>(domain)` returns the same fully-typed Proxy as the legacy
`edenClient<App>(domain)` (`EdenClient.Create<App>`), which is still exported for
back-compatibility.

### `resources` — Angular signal resources (mirrors `resource()`/`httpResource()`)

```typescript
import { createClient } from '@treaty/httpclient/client';
import { edenHttpResource } from '@treaty/httpclient/resources';

const client = createClient<App>('http://localhost:3000');

// `T` is INFERRED end-to-end from the Elysia route's 200 response type.
const users = edenHttpResource(() => client.users.get());

users.value();    // Signal<User[] | undefined>   (route response type, inferred)
users.status();   // Signal<'idle'|'loading'|'reloading'|'resolved'|'error'|'local'>
users.error();    // Signal<Error | undefined>
users.isLoading();
users.reload();

// Reactive params drive reloads:
const post = edenHttpResource(
  (id: number) => client.posts[id].get(),
  { params: () => selectedId() }
);

// Promise-client variant (backed by Angular's resource()):
import { createPromiseClient } from '@treaty/httpclient/client';
import { edenPromiseResource } from '@treaty/httpclient/resources';
const pclient = createPromiseClient<App>('http://localhost:3000');
const u = edenPromiseResource(() => pclient.users.get());
```

`edenHttpResource` (alias `edenResource`) is backed by Angular's `rxResource`;
`edenPromiseResource` is backed by `resource`. All return an Angular `ResourceRef<T>`.

## License

EdenClient is [IDGAF-1.0 licensed](./LICENSE).
