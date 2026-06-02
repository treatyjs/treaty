/*
 * angular-version-bump
 * --------------------
 * Bumps the Angular toolchain across EVERY package.json in the Treaty repo
 * (root + apps/* + libs/*), then reconciles the lockfile and gates on a green
 * Moon build + test.
 *
 * What it does, phase by phase:
 *   1. Discover  — find every package.json under the repo (root, apps/*, libs/*).
 *   2. Bump      — parallel(), one agent per package.json. Each agent rewrites the
 *                  @angular/*, @angular-devkit/*, @angular/cli, @angular/cdk,
 *                  @angular/material and the `typescript` peer to the target
 *                  versions (args.angular / args.typescript), touching deps,
 *                  devDeps AND peerDeps, and reports what it changed.
 *   3. Reconcile — a single BARRIER agent regenerates the lockfile (bun.lockb or
 *                  package-lock.json) and runs the Moon build + test gate
 *                  ('moon run :build' then 'moon run :test'). It loops on failure
 *                  (bounded while-loop, args.maxRounds) applying minimal fixes
 *                  each round until green or the cap is hit, logging what failed
 *                  per round. Returns final green / residual-failure status.
 *
 * NOTE: Nx was REMOVED from this repo in favour of Moon. Do NOT use
 * `nx run-many`; the gate is `moon run :build` / `moon run :test`.
 *
 * Invoke (defaults shown — runs with no args):
 *   Workflow({ name: "angular-version-bump" })
 *
 * Invoke with explicit targets:
 *   Workflow({
 *     name: "angular-version-bump",
 *     args: { angular: "22.0.0", typescript: "5.9.0", cdk: "22.0.0", maxRounds: 4 }
 *   })
 */

export const meta = {
  name: 'angular-version-bump',
  description:
    'Bump @angular/* + devkit + cli + cdk/material + the typescript peer to a target version across every package.json (root + apps/* + libs/*), then reconcile the lockfile and gate on a green Moon build + test, looping on failure until green or a max-rounds cap.',
  phases: [
    { title: 'Discover' },
    { title: 'Bump' },
    { title: 'Reconcile' },
  ],
}

const ROOT = 'd:/dev/treaty'

// ---- Targets (pinned via the `args` global, with sensible built-in defaults) ----
// Angular reference is vendored at tools/angular-ref; OXC is pinned at 0.133 in
// the Rust crates and is NOT touched here (this bump is the JS/TS toolchain only).
const TARGET_ANGULAR = (typeof args !== 'undefined' && args && args.angular) || '22.0.0'
const TARGET_TS = (typeof args !== 'undefined' && args && args.typescript) || '5.9.0'
const TARGET_CDK = (typeof args !== 'undefined' && args && args.cdk) || TARGET_ANGULAR
const MAX_ROUNDS = (typeof args !== 'undefined' && args && Number(args.maxRounds)) || 4

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

const DISCOVER_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['manifests'],
  properties: {
    manifests: {
      type: 'array',
      description: 'every package.json under the repo (root + apps/* + libs/*)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['path', 'name', 'hasAngular'],
        properties: {
          path: { type: 'string', description: 'absolute path to the package.json' },
          name: { type: 'string', description: 'the package "name" field, or the folder name if unnamed' },
          hasAngular: {
            type: 'boolean',
            description: 'true if any @angular/*, @angular-devkit/*, @angular/cli, @angular/cdk, @angular/material, or a typescript dep/peerDep is present',
          },
        },
      },
    },
  },
}

const BUMP_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['path', 'changed', 'edits', 'note'],
  properties: {
    path: { type: 'string', description: 'absolute path to the package.json that was processed' },
    changed: { type: 'boolean', description: 'true if the file was actually modified' },
    edits: {
      type: 'array',
      description: 'one entry per dependency version that was rewritten',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['dep', 'section', 'from', 'to'],
        properties: {
          dep: { type: 'string', description: 'the dependency name, e.g. @angular/core' },
          section: {
            type: 'string',
            enum: ['dependencies', 'devDependencies', 'peerDependencies'],
          },
          from: { type: 'string', description: 'the previous version range' },
          to: { type: 'string', description: 'the new version range' },
        },
      },
    },
    note: { type: 'string', description: 'anything notable: no-op, skipped a wildcard "*" workspace dep, left a non-Angular peer untouched, etc.' },
  },
}

const RECONCILE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['status', 'rounds', 'lockfile', 'roundLog', 'residualFailures'],
  properties: {
    status: {
      type: 'string',
      enum: ['GREEN', 'RESIDUAL_FAILURES', 'BLOCKED'],
      description: 'GREEN = build+test passed; RESIDUAL_FAILURES = cap hit with failures left; BLOCKED = could not even run the gate',
    },
    rounds: { type: 'number', description: 'how many build+test rounds were executed' },
    lockfile: { type: 'string', description: 'which lockfile was reconciled, e.g. bun.lockb or package-lock.json' },
    roundLog: {
      type: 'array',
      description: 'one entry per round describing what failed and what was attempted',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['round', 'gate', 'passed', 'summary'],
        properties: {
          round: { type: 'number' },
          gate: { type: 'string', enum: ['lockfile', 'build', 'test'] },
          passed: { type: 'boolean' },
          summary: { type: 'string', description: 'what failed and what fix was applied this round' },
        },
      },
    },
    residualFailures: {
      type: 'array',
      description: 'failures still present when the workflow stopped (empty if GREEN)',
      items: { type: 'string' },
    },
  },
}

// ---------------------------------------------------------------------------
// Phase 1: Discover
// ---------------------------------------------------------------------------

phase('Discover')

const discovery = await agent(
  `Find EVERY package.json under the Treaty repo at "${ROOT}" — the root one, plus all of them under apps/* and libs/* (any depth). Use Glob (e.g. "package.json", "apps/**/package.json", "libs/**/package.json") and ignore anything inside node_modules, dist, .moon/cache, target, or other build-output dirs.

For each manifest, Read it and report:
- path: the ABSOLUTE path to the package.json.
- name: its "name" field (fall back to the containing folder name if it has none).
- hasAngular: true if it declares ANY of these in dependencies, devDependencies, or peerDependencies: a package matching @angular/* , @angular-devkit/* , @angular/cli , @angular/cdk , @angular/material , or a "typescript" entry. Otherwise false.

Do NOT modify anything — discovery only. Return via the StructuredOutput tool.`,
  { label: 'discover:manifests', phase: 'Discover', schema: DISCOVER_SCHEMA, agentType: 'Explore' }
)

const manifests = (discovery && discovery.manifests) || []
const targets = manifests.filter(m => m && m.path)
log(`discovered ${manifests.length} package.json files; ${targets.filter(m => m.hasAngular).length} declare Angular/TS deps`)

// ---------------------------------------------------------------------------
// Phase 2: Bump (parallel — one agent per package.json)
// ---------------------------------------------------------------------------

phase('Bump')

function bumpPrompt(m, index) {
  // Vary the worktree branch suffix by array index so two agents never collide
  // on a branch name (avoids using a clock/random source, which is forbidden here).
  return `You are bumping the Angular/TypeScript toolchain in ONE package.json:
  ${m.path}    (name: ${m.name || 'unknown'}, index #${index})

TARGET VERSIONS (use these EXACT values; preserve the existing range PREFIX such as ^ or ~ when the original used one — e.g. "^21.2.15" -> "^${TARGET_ANGULAR}", "~5.9.0" -> "~${TARGET_TS}"):
- Every @angular/* package (core, common, compiler, compiler-cli, forms, router, animations, platform-browser, platform-browser-dynamic, language-service, etc.): "${TARGET_ANGULAR}".
- Every @angular-devkit/* package (build-angular, core, schematics, architect, etc.): "${TARGET_ANGULAR}".
- @angular/cli and @schematics/angular and @angular-eslint/* : "${TARGET_ANGULAR}".
- @angular/cdk and @angular/material : "${TARGET_CDK}".
- The "typescript" dependency (the Angular TS peer) : "${TARGET_TS}".

RULES:
1. Read the file first. Update matching entries in dependencies, devDependencies AND peerDependencies. peerDependency RANGES may be expressed as multi-version ranges (e.g. ">=21.0.0 <22.0.0") — widen/replace them so they ADMIT the new target (e.g. ">=${TARGET_ANGULAR}") rather than blindly overwriting with an exact pin; explain the choice in "note".
2. Do NOT touch internal workspace deps pinned to "*" or "workspace:*" (e.g. "@treaty/compiler": "*"). Leave non-Angular, non-typescript deps (vite, rxjs, zone.js, eslint, oxlint, jest, vitest, @types/node, @swc/*, volar, etc.) UNCHANGED — they are out of scope for this bump.
3. Do NOT reformat the whole file or reorder keys — make the MINIMAL textual edits to the version strings only (use the Edit tool, not a full rewrite). Keep JSON valid.
4. If nothing in this file matches the target set, make NO edit and report changed=false with note explaining it had no Angular/TS deps.
5. Do NOT run any install, lockfile, or build command — that happens in a later barrier stage. Editing the manifest text is the entire job here.

Report the path, whether you changed it, an "edits" array (one row per rewritten version with dep/section/from/to), and a short "note". Return via the StructuredOutput tool.`
}

const bumpResults = (await parallel(
  targets.map((m, index) => () =>
    agent(bumpPrompt(m, index), {
      label: `bump:${(m.name || m.path).slice(0, 28)}`,
      phase: 'Bump',
      schema: BUMP_SCHEMA,
      // Each manifest bump runs in its own worktree so parallel edits don't clash;
      // the reconcile barrier integrates them.
      isolation: 'worktree',
    })
  )
)).filter(Boolean)

const changedFiles = bumpResults.filter(r => r && r.changed)
const totalEdits = changedFiles.reduce((n, r) => n + ((r.edits && r.edits.length) || 0), 0)
log(`bumped ${changedFiles.length}/${bumpResults.length} package.json files (${totalEdits} version edits) -> angular@${TARGET_ANGULAR}, typescript@${TARGET_TS}, cdk/material@${TARGET_CDK}`)

// ---------------------------------------------------------------------------
// Phase 3: Reconcile (BARRIER — single agent, bounded retry loop)
// ---------------------------------------------------------------------------

phase('Reconcile')

const editDigest = changedFiles
  .map(r => `- ${r.path}: ${(r.edits || []).map(e => `${e.dep}(${e.section}) ${e.from}->${e.to}`).join(', ') || r.note}`)
  .join('\n') || '(no manifests were changed)'

const reconcile = await agent(
  `You are the BARRIER stage of an Angular version bump in the Treaty repo at "${ROOT}". The parallel bump agents have already rewritten the Angular/TypeScript versions in these package.json files:
${editDigest}

Your job: integrate those edits, reconcile the lockfile, and drive the build+test gate to GREEN — or report exactly what residual failures remain.

CONTEXT YOU MUST RESPECT:
- The build orchestrator is MOON, not Nx (Nx was REMOVED). The gate is: \`moon run :build\` then \`moon run :test\`. NEVER use \`nx run-many\` or any nx command.
- Treaty is a Rust/OXC Angular compiler. The Ivy compiler is 'treaty_ivy' (4 crates under libs/treaty-ivy/{core,template,decorators,facade}); 'libs/render3' no longer exists. OXC is pinned at 0.133 — do NOT change Rust/OXC versions; this is a JS/TS toolchain bump only.
- The lockfile is bun.lockb if present, else package-lock.json. Detect which exists at the repo root and reconcile THAT one (bun: \`bun install\`; npm: \`npm install\` / \`npm install --package-lock-only\`).

PROCEDURE (a bounded loop, at most ${MAX_ROUNDS} rounds):
Round structure for each round R (starting at 1):
  a) Reconcile the lockfile so it matches the bumped manifests (run the install for the detected package manager). If install fails to resolve the new Angular/TS versions, capture the resolver error verbatim in the round log, attempt a minimal fix (e.g. relax an over-tight peer range that the bump left, or align a stray @angular package the bump missed), and continue.
  b) Run \`moon run :build\`. Record pass/fail and, on failure, the first concrete error(s).
  c) If build passed, run \`moon run :test\`. Record pass/fail and the first concrete failure(s).
  d) If BOTH build and test passed -> status GREEN, stop the loop immediately.
  e) Otherwise apply the SMALLEST plausible fix for the observed failure (a missed @angular dep version, a peer-range conflict, a renamed Angular API surfaced by the build, a tsconfig/target tweak the new TS requires) and go to the next round.

LOOP BOUND: do at most ${MAX_ROUNDS} rounds. After the cap, if still not green, stop and report status RESIDUAL_FAILURES with the remaining failures listed. If you cannot run the gate at all (moon missing, install completely broken), report status BLOCKED.

Log every round: which gate (lockfile/build/test) ran, whether it passed, and a one-line summary of the failure + the fix you applied. Report the final status, the number of rounds executed, which lockfile you reconciled, the full per-round roundLog, and any residualFailures still outstanding. Return via the StructuredOutput tool.`,
  { label: 'reconcile:lockfile+moon-gate', phase: 'Reconcile', schema: RECONCILE_SCHEMA }
)

log(`reconcile finished: ${reconcile && reconcile.status} after ${reconcile && reconcile.rounds} round(s) on ${reconcile && reconcile.lockfile}`)

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

return {
  targets: { angular: TARGET_ANGULAR, typescript: TARGET_TS, cdk: TARGET_CDK, maxRounds: MAX_ROUNDS },
  manifestsDiscovered: manifests.length,
  manifestsChanged: changedFiles.length,
  versionEdits: totalEdits,
  bumps: bumpResults,
  reconcile,
  status: (reconcile && reconcile.status) || 'BLOCKED',
}
