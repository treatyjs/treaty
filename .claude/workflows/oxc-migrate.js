/*
 * oxc-migrate
 * -----------
 * Migrates Treaty's Rust/OXC modules to a target OXC version, ONE module at a
 * time, as an isolated 3-stage pipeline per module so parallel rewrites never
 * collide:
 *
 *   Stage 1 (Rewrite):  an agent rewrites the module to the target OXC version
 *                       in its OWN git worktree (opts.isolation:'worktree') with
 *                       a worktree-local CARGO_TARGET_DIR so concurrent module
 *                       rewrites do not contend the shared target/.
 *   Stage 2 (Check):    an agent runs `cargo check -p <crate>` for the module's
 *                       crate and reports pass/fail with the exact diagnostics
 *                       (schema).
 *   Stage 3 (Parity):   an ADVERSARIAL reviewer agent confirms behavior parity,
 *                       ESPECIALLY that DI codegen (the ɵfac factory + ɵprov /
 *                       ɵɵdefineInjectable / provider defs) emits byte-identical
 *                       output, and returns a verdict (schema {parityHeld,
 *                       concern, ...}).
 *
 * The pipeline runs once per Rust module. The module list comes from the `args`
 * global (args.modules) and falls back to a sensible built-in default covering
 * the treaty_ivy crates + the authoring front-ends that touch oxc_* APIs.
 *
 * Build orchestration is Moon (Nx was removed): `moon run :build` / `moon run :test`.
 *
 * INVOKE (no args -> built-in module list, OXC pinned at 0.133):
 *   Workflow({ name: "oxc-migrate" })
 *
 * INVOKE (override target + module subset):
 *   Workflow({
 *     name: "oxc-migrate",
 *     args: {
 *       oxc: "0.133",
 *       modules: [
 *         { crate: "treaty_ivy_core", path: "libs/treaty-ivy/core/src/expression_converter.rs" },
 *         { crate: "rust_authoring",  path: "apps/rust/authoring/src/angular/decorator.rs" }
 *       ]
 *     }
 *   })
 */

export const meta = {
  name: 'oxc-migrate',
  description: 'Per-Rust-module 3-stage pipeline (isolated-worktree rewrite to a target OXC version -> cargo check -> adversarial DI-codegen parity review) over a module list supplied via args with a built-in default',
  phases: [
    { title: 'Rewrite', detail: 'isolated worktree per module: rewrite the module to the target OXC version (worktree-local CARGO_TARGET_DIR) so parallel rewrites do not collide' },
    { title: 'Check', detail: 'run `cargo check -p <crate>` for the module crate and report pass/fail with exact diagnostics' },
    { title: 'Parity', detail: 'adversarial review: confirm behavior parity, ESPECIALLY that DI codegen (ɵfac factory + ɵprov/provider defs) output is byte-identical; return a verdict' },
  ],
}

const ROOT = 'd:/dev/treaty'

// ---- target OXC version (overridable via args, sensible built-in default) ----
const OXC_TARGET = (typeof args !== 'undefined' && args && args.oxc) ? String(args.oxc) : '0.133'

// ---- the module list (overridable via args.modules) -------------------------
// Each entry: { crate (the cargo package -p name), path (repo-relative file) }.
const DEFAULT_MODULES = [
  { crate: 'treaty_ivy_core',       path: 'libs/treaty-ivy/core/src/expression_converter.rs' },
  { crate: 'treaty_ivy_core',       path: 'libs/treaty-ivy/core/src/output_ast.rs' },
  { crate: 'treaty_ivy_template',   path: 'libs/treaty-ivy/template/src/template_parser.rs' },
  { crate: 'treaty_ivy_decorators', path: 'libs/treaty-ivy/decorators/src/component.rs' },
  { crate: 'treaty_ivy_facade',     path: 'libs/treaty-ivy/facade/src/linker.rs' },
  { crate: 'rust_authoring',        path: 'apps/rust/authoring/src/context.rs' },
  { crate: 'rust_authoring',        path: 'apps/rust/authoring/src/angular/decorator.rs' },
]

const MODULES = (typeof args !== 'undefined' && args && Array.isArray(args.modules) && args.modules.length)
  ? args.modules
  : DEFAULT_MODULES

// ---- schemas for every structured-returning agent ---------------------------

const CHECK_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['crate', 'path', 'command', 'passed', 'errorCount', 'warningCount', 'diagnostics'],
  properties: {
    crate: { type: 'string' },
    path: { type: 'string' },
    command: { type: 'string', description: 'the exact cargo check command that was run, e.g. "cargo check -p treaty_ivy_core"' },
    passed: { type: 'boolean', description: 'true iff cargo check exited 0 with zero errors' },
    errorCount: { type: 'number', description: 'number of compiler errors reported' },
    warningCount: { type: 'number', description: 'number of compiler warnings reported' },
    diagnostics: { type: 'string', description: 'verbatim error/warning text (or the head of it) so a fixer can act; empty string if clean' },
  },
}

const PARITY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['crate', 'path', 'parityHeld', 'diCodegenUnchanged', 'concern', 'evidence'],
  properties: {
    crate: { type: 'string' },
    path: { type: 'string' },
    parityHeld: { type: 'boolean', description: 'true iff observable behavior + emitted output are unchanged by the OXC rewrite' },
    diCodegenUnchanged: { type: 'boolean', description: 'true iff the DI factory (ɵfac) + provider defs (ɵprov/ɵɵdefineInjectable) emit byte-identical output vs before the rewrite' },
    concern: { type: 'string', description: 'the single most serious parity risk found; empty string if none' },
    evidence: { type: 'string', description: 'concrete proof: parsed-output diffs, test names, golden-file comparisons. Cite what was actually run/read; NO regex over emitted code.' },
  },
}

const REWRITE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['crate', 'path', 'branch', 'rewritten', 'symbolsTouched', 'notes'],
  properties: {
    crate: { type: 'string' },
    path: { type: 'string' },
    branch: { type: 'string', description: 'the worktree branch the rewrite was committed on' },
    rewritten: { type: 'boolean', description: 'true iff the module was changed to target the new OXC version (false if already compatible)' },
    symbolsTouched: { type: 'string', description: 'the oxc_* symbols / call-sites that were migrated, comma-separated' },
    notes: { type: 'string', description: 'anything the Check/Parity stages need to know' },
  },
}

// ---- shared base instructions for every agent -------------------------------

const BASE = [
  'Treaty = a Rust/OXC Angular compiler. The Ivy compiler is treaty_ivy, split into 4 crates under libs/treaty-ivy/{core,template,decorators,facade} (libs/render3 no longer exists). Authoring front-ends live under apps/rust/authoring/src. The Angular reference is vendored at tools/angular-ref. Build orchestrator is Moon, NOT Nx: use `moon run :build` / `moon run :test` (never `nx run-many`).',
  'OXC is being migrated to target version ' + OXC_TARGET + '. The migration crib lives at migration/OXC-MIGRATION-CRIB.md — consult it for symbol renames/moves.',
  'PRODUCTION QUALITY. NO stubs/placeholders/TODO. End by calling the StructuredOutput tool exactly once. ZERO marker words in code.',
  'HARD RULE: NEVER verify emitted/compiled code with regex (false positives). Verify by parsing the AST (oxc in a Rust test) or by printing the real output and reading it.',
].join('\n')

const ISOLATION = [
  'ISOLATION: you run in your OWN git worktree (isolation:worktree). Before ANY cargo invocation set a worktree-local CARGO_TARGET_DIR (PowerShell: $env:CARGO_TARGET_DIR = "$PWD/.target-oxc"; bash: export CARGO_TARGET_DIR="$PWD/.target-oxc") so your build does NOT contend the shared workspace target/ that sibling module pipelines hold. Commit your work in the worktree on a branch; the orchestrator merges it.',
].join('\n')

function label(prefix, m) {
  // Keep labels short + deterministic (no clock/random): derive from the path tail.
  const tail = (m.path || '').split('/').slice(-2).join('/')
  return prefix + ':' + (m.crate || '') + '/' + tail
}

// One isolated 3-stage pipeline per module. `pipeline()` runs the stages in
// order, threading each stage's structured result into the next.
async function migrateModule(m, idx) {
  return pipeline(
    // ----- Stage 1: Rewrite (isolated worktree) -----
    {
      title: 'Rewrite',
      run: () => agent(
        [
          BASE,
          ISOLATION,
          'STAGE 1 (REWRITE). Module #' + idx + ': crate `' + m.crate + '`, file "' + ROOT + '/' + m.path + '".',
          'Rewrite ONLY this module to target OXC ' + OXC_TARGET + '. Read the file, identify every oxc_* / oxc_ast / oxc_syntax / oxc_allocator / oxc_span symbol and call-site it uses, and migrate each to the ' + OXC_TARGET + ' API (renamed enums/builders, moved modules, changed signatures, allocator/visit changes). Preserve behavior EXACTLY — this is a mechanical version migration, not a refactor. Do not touch any other file unless a shared type signature forces a minimal companion edit, and note it if so. Commit the change in your worktree on a clearly named branch. If the module is already compatible with ' + OXC_TARGET + ', set rewritten=false and explain. RETURN crate, path, branch, rewritten, symbolsTouched, notes via StructuredOutput.',
        ].join('\n'),
        { label: label('rewrite', m), phase: 'Rewrite', schema: REWRITE_SCHEMA, isolation: 'worktree' }
      ),
    },
    // ----- Stage 2: Check -----
    {
      title: 'Check',
      run: (rewrite) => agent(
        [
          BASE,
          ISOLATION,
          'STAGE 2 (CHECK). Prior REWRITE result: ' + JSON.stringify(rewrite).slice(0, 1400),
          'In the SAME worktree (set the worktree-local CARGO_TARGET_DIR first), run exactly `cargo check -p ' + m.crate + '` for the crate that owns "' + m.path + '". Capture the REAL exit status and the verbatim diagnostics — do not fabricate green. If it fails, report the exact errors so a fixer can act (you MAY apply an obvious one-line follow-up fix to the rewritten module and re-run, but do not paper over a real semantic break). passed=true ONLY if cargo check exits 0 with zero errors. RETURN crate, path, command, passed, errorCount, warningCount, diagnostics via StructuredOutput.',
        ].join('\n'),
        { label: label('check', m), phase: 'Check', schema: CHECK_SCHEMA, isolation: 'worktree' }
      ),
    },
    // ----- Stage 3: Parity (adversarial) -----
    {
      title: 'Parity',
      run: (rewrite, check) => agent(
        [
          BASE,
          ISOLATION,
          'ADVERSARIAL VERIFICATION. Your job is to find a parity break, not to rubber-stamp.',
          'STAGE 3 (PARITY). Prior REWRITE: ' + JSON.stringify(rewrite).slice(0, 900) + ' | Prior CHECK: ' + JSON.stringify(check).slice(0, 900),
          'Confirm the OXC ' + OXC_TARGET + ' rewrite of crate `' + m.crate + '` (module "' + m.path + '") changed NOTHING observable. Verify by running the real golden/parity tests for the crate in the worktree (worktree-local CARGO_TARGET_DIR; prefer `moon run :test` for the affected targets, or `cargo test -p ' + m.crate + '`) and by compiling representative authoring sources through the affected path and PARSING the emitted output (oxc AST / print-and-read) against the pre-rewrite output. NEVER regex over emitted code.',
          'CRITICAL: pay special attention to DI codegen. The emitted dependency-injection output — the ɵfac factory function and the provider defs (ɵprov / ɵɵdefineInjectable / ɵɵdefineInjector, useClass/useFactory/useValue/useExisting, multi providers, forwardRef thunks) — MUST be byte-identical before vs after the rewrite. Diff the emitted factory + provider defs explicitly. Set diCodegenUnchanged=false (and parityHeld=false) if ANY DI emission differs, even whitespace/ordering. Report the single most serious concern and the concrete evidence (test names, parsed diffs, golden comparisons). RETURN crate, path, parityHeld, diCodegenUnchanged, concern, evidence via StructuredOutput.',
        ].join('\n'),
        { label: label('parity', m), phase: 'Parity', schema: PARITY_SCHEMA, isolation: 'worktree' }
      ),
    },
  )
}

// Drive all module pipelines. Each module owns its own worktree, so they can run
// concurrently without colliding; parallel() fans them out.
phase('Rewrite')
log('oxc-migrate: ' + MODULES.length + ' module(s) -> OXC ' + OXC_TARGET)

const results = (await parallel(
  MODULES.map((m, idx) => () => migrateModule(m, idx))
)).filter(Boolean)

phase('Check')
log('cargo check completed for ' + results.length + ' module pipeline(s)')

phase('Parity')

// Aggregate the final verdicts. A module is GREEN only if it compiled AND parity
// (incl. DI codegen) held.
const summary = results.map((r, idx) => {
  const m = MODULES[idx] || {}
  const rewrite = (r && r.Rewrite) || (r && r.rewrite) || {}
  const check = (r && r.Check) || (r && r.check) || {}
  const parity = (r && r.Parity) || (r && r.parity) || {}
  return {
    crate: m.crate,
    path: m.path,
    rewritten: !!rewrite.rewritten,
    checkPassed: !!check.passed,
    parityHeld: !!parity.parityHeld,
    diCodegenUnchanged: !!parity.diCodegenUnchanged,
    green: !!(check.passed && parity.parityHeld && parity.diCodegenUnchanged),
    concern: parity.concern || '',
  }
})

const green = summary.filter(s => s.green)
const diRegressions = summary.filter(s => s.checkPassed && !s.diCodegenUnchanged)
log('green: ' + green.length + '/' + summary.length + '; DI-codegen regressions: ' + diRegressions.length)

return {
  oxcTarget: OXC_TARGET,
  totalModules: MODULES.length,
  greenCount: green.length,
  diRegressions,
  summary,
  results,
}
