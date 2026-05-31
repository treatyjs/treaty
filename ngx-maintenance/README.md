# ngx-maintenance

A **deterministic, no-AI** GitHub bot that keeps Angular libraries current
(v9 -> latest, including View Engine -> Ivy), opens migration PRs for stale
libraries, and **takes over** clearly-unmaintained ones into maintained forks.

This is a [Turborepo](https://turbo.build) living inside the Treaty monorepo. It
supports Treaty authoring and the Treaty compiler, but is otherwise independent
of `libs/` and `apps/`.

## Why no AI

Migrations are **fully automated and deterministic** — never "ask an LLM". The
engine drives Angular's OWN `ng update` migration schematics (each
`@angular/core@N` ships its migrations) chained `vN -> vN+1 -> ... -> latest`,
plus the View-Engine -> Ivy path for the v8-v12 window, plus a curated set of
deterministic codemods (oxc / ts-morph) for the gaps the official schematics do
not cover. Every step is verified by **install + build + test**. A human — never
a model — reviews any step that fails.

This keeps the bot cheap to run, reproducible, and auditable: the same input
repository at the same Angular major always produces the same migration plan.

## How it works

1. **Opt-in + discovery.** People ADD their library to the registry. The bot
   also DISCOVERS libraries that are behind the latest Angular major AND have no
   commit in over six months, then opens an issue suggesting they opt in (with a
   sample PR).
2. **App installed => auto-roll.** Once a repo installs the GitHub App, every
   new Angular release triggers an automatic migration PR.
3. **Deterministic chain.** For a library at vX, plan vX+1..latest. Each step is
   the official `ng update @angular/core@N @angular/cli@N --migrate-only
   --from=N-1 --to=N` plus peer bumps; the v8-v12 window also runs the VE -> Ivy
   migration and the Ivy partial-compilation switch. Curated codemods fill known
   gaps. Verify after every step.

## Takeover policy

If a migration PR is **not merged within two weeks** AND the library is clearly
unmaintained (no recent activity + maintainer non-response), the takeover policy
fires:

- fork the repository,
- run the deterministic migration chain,
- publish under the **`@ngx-maintenance/<name>`** npm scope with an explicit
  compatibility-only warning banner:

  > This is a compatibility-only ngx-maintenance fork. It tracks Angular
  > compatibility ONLY — not bug fixes or new features. Please migrate to a
  > supported alternative.

- and spin the fork into its **OWN repo** (each taken-over library becomes a
  standalone repo; the tooling stays here).

Both conditions are required: the two-week merge window must elapse AND the
abandonment signals must hold. The decision is a pure function of timestamps and
activity — no AI.

## Optional: migrate a fork to Treaty

A maintained fork can OPT IN to an additional, deterministic migration onto
Treaty authoring/packaging. This is strictly opt-in and runs as one extra step
**after** the Angular migration chain succeeds and **before** the PR opens:

- `compat` mode switches only the packaging path (build with `treaty-packagr`,
  authoring unchanged);
- `enhanced` mode also applies the fixed structural authoring transforms
  (`treaty-decorator-rewrite`, `treaty-template-binding`,
  `treaty-package-manifest`).

The step lives behind the same injected boundary as everything else
(`TreatyMigrationStep`), so it is fully fake-testable and runs through the one
process shell the bot already owns. A library is run through it **only** when
its npm name is listed in `treatyOptIn` AND the deployment wired the Treaty
adapter (`enableTreaty`). For every other library nothing Treaty-related runs
and the flow is byte-for-byte unchanged. If the step fails, the library is
reported `treaty-failed` and **no PR opens** — a human reviews, never an LLM.

Enable it on the CLI by listing opted-in packages:

```sh
bun run apps/bot/src/cli.ts poll \
  --treaty-opt-in @acme/widget \
  --treaty-opt-in @acme/forms \
  --treaty-mode enhanced
```

Passing any `--treaty-opt-in` automatically wires the production Treaty adapter.

## Running the bot (cron)

The runnable bot is a CLI in `apps/bot` driven by GitHub Actions on a schedule —
the dependabot-style cron entry lives at
[`.github/workflows/maintenance.yml`](.github/workflows/maintenance.yml). Two
jobs run on independent crons (and on manual dispatch):

| Job    | Cron                | What it does                                                                 |
| ------ | ------------------- | ---------------------------------------------------------------------------- |
| `poll` | `0 6 * * *` (daily) | Full cycle: discover stale opted-in libs -> migrate -> open PRs (idempotent) -> evaluate the 2-week takeover timer. |
| `scan` | `0 7 * * 1` (weekly)| Discovery-only pass that surfaces stale, unregistered libs as opt-in suggestions; carries no outstanding takeover PRs. |

Each job checks out, sets up Bun, `bun install`, `bun run build` (output goes to
the gitignored `dist/`), then invokes the CLI:

```sh
# the daily cron is equivalent to:
bun run apps/bot/src/cli.ts poll --manifest registry.json --org acme --org widgets
# the weekly cron is equivalent to:
bun run apps/bot/src/cli.ts scan --manifest registry.json --org acme
```

The CLI commands:

| Command     | Meaning                                                                          |
| ----------- | -------------------------------------------------------------------------------- |
| `poll`      | Drive one cycle through the **pure scheduler**. On a stateless CI runner the fresh state is always due, so this is the cron entry point; a long-lived host threads `state` back in and the scheduler gates re-runs. |
| `scan`      | Drive a discovery cycle with **no** outstanding takeover PRs (PRs only for newly-stale libs). |
| `run-cycle` | Drive one cycle directly (the original, scheduler-free entry).                   |

`poll` always finds a cycle due on a fresh CI runner because the scheduler state
is not persisted between Action runs; the scheduler exists so a long-lived host
(or a future stateful runner) can rate-limit cycles without changing the logic.

### Configuration

Secrets and repository variables on the `ngx-maintenance` repo drive the run:

| Name                | Kind     | Purpose                                                                        |
| ------------------- | -------- | ------------------------------------------------------------------------------ |
| `MAINTENANCE_TOKEN` | secret   | PAT / GitHub App token with cross-org PR/issue/fork write scope. Falls back to the run's `GITHUB_TOKEN` when unset. |
| `WATCHED_ORGS`      | variable | Space-separated GitHub orgs the discovery enumerates (each becomes a `--org`).  |
| `LATEST_ANGULAR`    | variable | The latest published Angular major the chain targets (defaults to `22`).        |

Thresholds are code defaults in `@ngx-maintenance/orchestrator`'s `resolveConfig`
and are operator-tunable per the `BotConfig` interface:

- `stalenessWindowMs` — inactivity window before a behind-latest lib is "stale"
  (default six months);
- `takeoverWindowMs` — unmerged-PR window before an abandoned lib is taken over
  (default two weeks);
- `targetAngular` / `baseBranch` / `cloneDepth` — migration target, PR base
  branch, and shallow-clone depth;
- `treatyOptIn` / `treatyMode` — the optional Treaty path (see above).

A manual `workflow_dispatch` run can pick the `command` (`poll` or `scan`) and
override the watched `orgs` for a one-off pass (e.g. after onboarding a new org).

## Layout

| Path                          | Responsibility                                                                 |
| ----------------------------- | ------------------------------------------------------------------------------ |
| `apps/bot`                    | The GitHub App: webhook handlers (installation, push, Angular release), the scheduler, PR/issue creation and the 2-week takeover timer. |
| `packages/registry`           | The opted-in library registry manifest + stale-lib discovery.                  |
| `packages/migration-engine`   | The deterministic migration chain: planner + step runner + curated codemods.   |
| `packages/staleness-detector` | The pure staleness predicates + metadata-source interface + thresholds.        |
| `packages/orchestrator`       | The deterministic end-to-end cycle wiring every package through one injectable adapter boundary, plus the pure scheduler. |
| `packages/takeover`           | The takeover policy engine + fork/spec builder.                                |
| `packages/treaty-support`     | Optional migration of forks to Treaty authoring + treaty-packagr packaging.    |
| `angular-pkgs/`               | Staging area for maintained forks before they split into their own repos.      |

## Tooling

- **Typecheck:** [tsgo](https://github.com/microsoft/typescript-go)
  (`node_modules/.bin/tsgo --noEmit`) — not `tsc`.
- **Lint:** [oxlint](https://oxc.rs) via `.oxlintrc.json`.
- **Tasks:** Turborepo (`turbo run typecheck|lint|build|test`).

External infrastructure dependencies (`octokit`, `@angular/cli`) are referenced
**structurally** via ambient declarations in `types/ambient.d.ts`, so typecheck
passes without installing them. The runtime app installs the real packages out
of band; hosting, secrets, npm publishing, and per-takeover repo creation are
scripted but run out-of-band.

## Commands

```sh
# from ngx-maintenance/
turbo run typecheck   # tsgo --noEmit across all packages
turbo run lint        # oxlint across all packages
turbo run test        # vitest run across all packages
turbo run build       # emit dist/ (gitignored) across all packages
```
