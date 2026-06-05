# ngx-maintenance — automated Angular library maintenance bot (detailed plan)

Evolves the original idea (https://github.com/danielglejzner/ngx-maintenance — a ViewEngine→Ivy
compatibility initiative, was Nx) into a **deterministic, no-AI GitHub bot** that keeps Angular
libraries current (v9 → latest, incl. View Engine → Ivy), opens PRs for stale libs, and **takes over**
clearly-unmaintained libs into maintained forks. Lives in THIS repo as a **Turborepo** (not Nx).
Supports Treaty authoring + the Treaty compiler.

## Hard constraints
- **NO AI / LLM** — too costly, must be fully automated. Migrations are DETERMINISTIC: drive Angular's
  OWN `ng update` migration schematics (each `@angular/core@N` ships its migrations) chained
  vN→vN+1→…→latest, plus the View-Engine→Ivy path (Angular's VE→Ivy migration + ngcc-era handling for
  the v9–v12 window), plus a curated set of deterministic codemods (oxc/ts-morph) for the gaps the
  official schematics don't cover. Verify by install + build + test, never by "asking an LLM".
- **Opt-in + discovery**: people ADD their lib (registry). The bot also DISCOVERS stale libs (behind
  latest Angular AND no commit in >6 months) and opens an issue SUGGESTING they add the app + a sample PR.
- **App installed ⇒ auto-roll**: once a repo installs the GitHub App, every new Angular release triggers
  an automatic migration PR for them.
- **Takeover**: if a migration PR is not merged within **2 weeks** AND the lib is clearly unmaintained,
  pull the lib into a maintained fork — published under an `@ngx-maintenance/<name>` scope with an
  explicit "compatibility-only, not bugs/features, migrate to a supported alternative" NPM warning —
  and **spin it into a NEW repo** (each taken-over lib is its own repo; the tooling stays here).
- **Tooling**: Turborepo; TS typecheck via **tsgo**, lint via **oxlint** ([[treaty-tooling-tsgo-oxc]]);
  Treaty is a compiler not a host.

## Architecture (Turborepo: `ngx-maintenance/`)
- `apps/bot` — the **GitHub App** (octokit/probot): webhook handlers (installation, push, release of
  `@angular/core`), the scheduler (cron: re-scan registry + discovery), PR/issue creation, and the
  2-week takeover timer. Deployment (hosting/secrets) is out-of-band; the app LOGIC + handlers are here.
- `packages/registry` — the opted-in lib registry (a manifest: repo URL, npm name, current Angular
  version, app-installed flag) + discovery (npm/GitHub queries for libs behind latest + stale >6mo).
- `packages/migration-engine` — the deterministic migration CHAIN: given a cloned repo at Angular vX,
  plan and run vX→…→latest (each step = the official `ng update @angular/core@N --migrate-only` +
  framework peers), handle the VE→Ivy transition, run curated codemods for gaps, then `install + build
  + test` to verify each step; produce a structured result (success | failed-at-step + logs). NO AI.
- `packages/takeover` — the policy engine: detect "unmaintained" (no merge in 2 weeks + no recent
  activity + maintainer non-response), fork→apply migration→publish `@ngx-maintenance/<name>` with the
  warning banner, and emit a "new repo" spec (each takeover = its own repo).
- `packages/treaty-support` — optionally migrate the lib to Treaty's enhanced authoring + build/package
  it with the Treaty compiler/treaty-packagr (so maintained forks can adopt Treaty); supports all
  Treaty authoring.
- `angular-pkgs/` — the maintained forks staged in-repo before they are split into their own repos.

## Migration chain detail (the hard part, no AI)
1. Detect the lib's current Angular major (from package.json peers + angular.json/ng-package.json).
2. For each major N from current+1..latest: `ng update @angular/core@N @angular/cli@N` with
   `--migrate-only --from=N-1 --to=N` so the OFFICIAL Angular migrations run; bump peer deps; for the
   v8→v9→v12 window apply the View-Engine→Ivy migration + Ivy partial-compilation switch.
3. After each major: `npm i` + `ng build` (or treaty-packagr) + tests; if a step fails, run the curated
   deterministic codemod for that known breakage (oxc/ts-morph), retry; if still failing, stop and
   report the exact failing step (a human reviews — never an LLM).
4. Emit the migrated branch + a changelog of which official migrations + codemods ran.

## Phases / fan-out (one workflow, many agents — disjoint files within the new dir)
- **P1 Scaffold**: `ngx-maintenance/` Turborepo (turbo.json, root package.json, tsconfig/tsgo/oxlint),
  the package skeletons above.
- **P2 (parallel)**: migration-engine (chain planner + runner), registry+discovery, takeover policy,
  bot/octokit handlers — disjoint packages, parallel agents.
- **P3 Verify**: tsgo + oxlint + unit tests (chain planner produces the right vN steps incl. VE→Ivy;
  takeover policy fires at 2 weeks + unmaintained; discovery flags >6mo/behind).

## Notes
- The actual GitHub App hosting + npm publishing + per-takeover repo creation need credentials/infra —
  those steps are scripted but run out-of-band; everything testable (planning, policy, handlers, codemods)
  is built + unit-tested here.
- This is a NEW top-level area (`ngx-maintenance/`), disjoint from render3 / libs/treaty / apps/repl, so
  it parallelizes with other waves.
