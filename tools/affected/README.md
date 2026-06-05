# @treaty-tools/affected

Nx/Turborepo-style **affected** at **federated-module** granularity for Treaty.

Treaty's unit of deployment is the federated module: the host container, every
lazy feature route (a route-as-remote), and every workspace library — each
independently versioned, deployable, and rollback-able (see
`@treaty/module-federation`). When a commit changes some files, CI should not
rebuild / re-test / re-deploy the whole app; only the modules actually
**affected**:

- the modules whose source changed (the *directly changed* set), **plus**
- every module that **transitively depends** on one of them (a shared-lib change
  fans out to its dependents).

This tool computes that set deterministically and with no AI, so CI scopes
compile/test/deploy to exactly that federation.

## The model

A **project graph** of nodes (federated modules) and edges:

```jsonc
{
  "nodes": [
    { "id": "host:shell", "kind": "host", "paths": ["src/app"],
      "dependsOn": ["lib:ui", "route:dashboard"] },
    { "id": "route:dashboard", "kind": "route", "paths": ["src/app/dashboard"],
      "dependsOn": ["lib:ui"] },
    { "id": "route:settings", "kind": "route", "paths": ["src/app/settings"] },
    { "id": "lib:ui", "kind": "lib", "paths": ["libs/ui"] }
  ]
}
```

- `id` — the module's stable identity (the federation exposes key, or host name).
- `kind` — `host` | `route` | `lib`.
- `paths` — the source path PREFIXES the module OWNS. A changed file is attributed
  to the node with the **longest** matching owning prefix (nearest module wins).
- `dependsOn` — the modules this one imports. A change to a dependency fans out.

`@treaty/module-federation` already enumerates the *nodes* (via
`federatedModules(...)`) but deliberately does not model the *edges*. Seed the
nodes from its JSON output and layer the edges on with `--federation`:

```jsonc
{
  "modules": [ /* federatedModules(...) output */ ],
  "dependencies": { "./routes/dashboard": ["./libs/ui"] },
  "extraPaths":   { "./routes/dashboard": ["./src/shared/dashboard-utils"] }
}
```

## CLI (what CI invokes)

```sh
# diff the working tree against a base, against a graph file:
treaty-affected --graph graph.json --base origin/main...HEAD --format ids

# pipe a precomputed diff in:
git diff --name-only origin/main | treaty-affected --graph graph.json --files -

# seed nodes from federation output + layer edges:
treaty-affected --federation fed.json --files changed.txt --format ids

# treat global inputs as affecting everything:
treaty-affected --graph graph.json --files changed.txt --global pnpm-lock.yaml --global tsconfig.base.json
```

Flags: `--graph <file|->`, `--federation <file|->`, `--files <file|->`,
`--base <rev>`, `--global <prefix>` (repeatable), `--format json|ids` (default
`json`), `--fail-on-empty` (exit non-zero when nothing is affected), `-h`.

`--format ids` prints one affected module id per line (pipe straight into a
build/deploy `--projects` / include filter). `--format json` prints
`{ affected, affectedIds, directlyChangedIds, unmatchedFiles, count }`.

`unmatchedFiles` is the changed files owned by no module (a root/CI/tooling
change). The walk never invents modules for them; declare such paths with
`--global` if they should force a full build.

## Behavior

| Change                         | Affected                                      |
| ------------------------------ | --------------------------------------------- |
| a leaf module's own source     | only that module                              |
| a shared lib's source          | the lib **plus all its transitive dependents** |
| a file owned by no module      | nothing (reported in `unmatchedFiles`)        |
| a `--global` path              | every module                                  |

Dependency cycles terminate (visited-guard) and yield the whole cycle.

## Develop

Standalone, dependency-free (Node built-ins only). Uses the repo's dev tools:

```sh
# from the repo root:
node_modules/.bin/tsgo --noEmit -p tools/affected/tsconfig.json   # typecheck
node_modules/.bin/oxlint -c .oxlintrc.json tools/affected/src tools/affected/bin tools/affected/test
node_modules/.bin/vitest run --config tools/affected/vitest.config.ts
node_modules/.bin/tsgo -p tools/affected/tsconfig.json            # build -> dist/
```
