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


## License

EdenClient is [IDGAF-1.0 licensed](./LICENSE).
