# Treaty

Is Angular in perfect harmony in the Bun runtime using Elysia.js and Surrealdb

- Angular v17.2.0
- Bun 1.0.26
- Surrealdb.node 0.3
- Elysia 0.8

## Extra features

- Custom elysia (eden/treaty) that uses httpClient under the hood
- It's Zoneless together with SSR
- Server running with bun
- Tests running with bun

## Getting started

- `bun i`
- `bun run build`
- `bun run server.ts`

## Getting started watching

- `bun run watch` - Known bottleneck it takes roughly 1s to build per change
- `bun run --watch server.ts`

## Unit testing

- `bun test`

### This is still very much POC dont use in production
