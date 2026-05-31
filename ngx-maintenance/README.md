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

## Layout

| Path                          | Responsibility                                                                 |
| ----------------------------- | ------------------------------------------------------------------------------ |
| `apps/bot`                    | The GitHub App: webhook handlers (installation, push, Angular release), the scheduler, PR/issue creation and the 2-week takeover timer. |
| `packages/registry`           | The opted-in library registry manifest + stale-lib discovery.                  |
| `packages/migration-engine`   | The deterministic migration chain: planner + step runner + curated codemods.   |
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
```
