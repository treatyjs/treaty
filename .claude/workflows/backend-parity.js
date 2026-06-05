/*
 * backend-parity — the "migrate any future change" engine for Treaty's
 * multi-backend compiler. It keeps the lagging compiler backends 1:1 with the
 * leading OXC backend by driving the deterministic NO-AI harness at
 * `tools/backend-parity` (a detached Rust workspace, mirroring tools/render3-sync).
 *
 * Treaty is a Rust/OXC Angular (Ivy) compiler. OXC is the DEFAULT, leading backend
 * and the reference; the lagging sides that must be kept byte-identical are:
 *   - the SWC backend (libs/treaty-ivy/core/src/output/emitter_swc.rs + the
 *     ParseBackend impl in libs/treaty-ivy/backend/src/parse.rs), and
 *   - the React-emit target (matched here too per migration/SWC-BACKEND-PLAN.md §3.3).
 * The 1:1 invariant: the SAME @Component / partial-declaration / .treaty/JSX input
 * must emit byte-identical Ivy (or the matched React output) under every backend.
 *
 * Flow:
 *   (Detect)  An agent runs `backend-parity drift` + `backend-parity migrate-plan`
 *             and parses the JSON: the drift set (regressions vs the committed
 *             baseline) plus the structured port-tasks describing what each lagging
 *             backend must implement to match OXC.
 *   (Migrate) One agent PER port-task (worktree-isolated so parallel edits don't
 *             collide) implements the matching change on the lagging side and re-runs
 *             the harness for ITS slice (`backend-parity parity --only <id>`).
 *   (Verify)  A final agent re-runs `backend-parity parity` over the whole corpus and
 *             reports whether the backends are 1:1 again (zero drift, zero open tasks).
 *
 * Invoke with:
 *   Workflow({ name: "backend-parity" })
 * Or override the harness/manifest/target via args (defaults are built in):
 *   Workflow({ name: "backend-parity", args: {
 *     manifest: "tools/backend-parity/Cargo.toml", // detached workspace manifest
 *     pkg: "backend-parity",                        // cargo package name
 *     maxTasks: 24,                                 // cap on fanned-out port-tasks
 *     targets: ["swc", "react"],                    // lagging backends to migrate
 *   }})
 *
 * NOTE: workflow scripts run with the nondeterministic clock/random builtins
 * disabled (Date.now()/Math.random() throw here). Anything that needs to "vary"
 * (labels, slice ids, fallbacks) is varied by ARRAY INDEX, never by time/random.
 */

export const meta = {
  name: 'backend-parity',
  description: 'Drive the tools/backend-parity harness to detect drift / unimplemented-backend port-tasks, fan out one agent per task to migrate the lagging side (swc backend / React-emit target), then verify the backends are byte-identical 1:1 again',
  phases: [
    { title: 'Detect', detail: 'run `backend-parity drift` + `migrate-plan`, parse the JSON drift set + port-tasks' },
    { title: 'Migrate', detail: 'one worktree-isolated agent per port-task implements the lagging-side change and re-runs its harness slice' },
    { title: 'Verify', detail: 'one agent re-runs `backend-parity parity` over the whole corpus and reports 1:1 status' },
  ],
}

const ROOT = 'd:/dev/treaty'

// --- Pins / config (overridable via the `args` global; built-in defaults so the
// --- script runs with no args). tools/backend-parity is its OWN detached
// --- [workspace], so the manifest-path form is the robust invocation; the
// --- friendly `-p <pkg>` form is documented in the prompts as the plan's spelling.
const MANIFEST = (args && args.manifest) || 'tools/backend-parity/Cargo.toml'
const PKG = (args && args.pkg) || 'backend-parity'
const MAX_TASKS = (args && Number(args.maxTasks)) || 24
const DEFAULT_TARGETS = ['swc', 'react']
const TARGETS = (args && Array.isArray(args.targets) && args.targets.length)
  ? args.targets.filter(t => DEFAULT_TARGETS.includes(t))
  : DEFAULT_TARGETS

// The harness commands, both spellings. `-p <pkg>` is what the plan/spec writes;
// `--manifest-path` is the form that works from the root for a detached workspace.
const RUN = `cargo run --manifest-path ${MANIFEST} --`
const RUN_P = `cargo run -p ${PKG} --`
const CMD_DRIFT = `${RUN} drift`
const CMD_MIGRATE_PLAN = `${RUN} migrate-plan`
const CMD_PARITY = `${RUN} parity`

// Where the lagging-side code lives, by target. Used to point the migrate agents at
// the right Rust files (grounded in migration/SWC-BACKEND-PLAN.md §3 + §7).
const TARGET_INFO = {
  swc: {
    title: 'SWC backend',
    blurb: 'second parser/codegen engine that must emit byte-identical Ivy to OXC',
    files: [
      `${ROOT}/libs/treaty-ivy/core/src/output/emitter_swc.rs`,
      `${ROOT}/libs/treaty-ivy/backend/src/parse.rs`,
      `${ROOT}/libs/treaty-ivy/backend/src/lib.rs`,
    ],
    feature: 'swc',
    plan: 'migration/SWC-BACKEND-PLAN.md (§2 oxc→swc mapping, §3 trait/feature seam, §5 phased rollout)',
  },
  react: {
    title: 'React-emit target',
    blurb: 'Angular/Treaty → React emit target that must be matched against OXC per the plan §3.3',
    files: [
      `${ROOT}/libs/treaty-ivy/core/src/output/`,
      `${ROOT}/apps/rust/authoring/src/jsx/react.rs`,
    ],
    feature: 'react',
    plan: 'migration/SWC-BACKEND-PLAN.md §3.3 (the React-emit work must be matched in the parity harness too)',
  },
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

// One port-task: a concrete change the lagging backend must implement to match OXC.
const PORT_TASK_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['id', 'target', 'title', 'detail'],
  properties: {
    id: { type: 'string', description: 'stable kebab id for the task (used as the harness slice filter + agent label)' },
    target: { type: 'string', description: 'which lagging backend this task migrates: e.g. "swc" or "react"' },
    title: { type: 'string', description: 'one-line description of the change' },
    detail: { type: 'string', description: 'what the lagging side must implement to match OXC (the oxc→swc / oxc→react API row, the diverging fixture, or the missing instruction)' },
    fixtures: { type: 'array', description: 'fixture ids in the corpus this task affects (for the harness slice filter)', items: { type: 'string' } },
    files: { type: 'array', description: 'absolute Rust paths the change is expected to touch', items: { type: 'string' } },
    source: { type: 'string', description: 'where this task came from: "drift" (regression vs baseline) or "migrate-plan" (unimplemented-backend port-task)' },
  },
}

const DETECT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['ran', 'driftCount', 'taskCount', 'tasks', 'rawSummary'],
  properties: {
    ran: { type: 'boolean', description: 'did the harness actually execute on the oxc side over the corpus?' },
    harnessExists: { type: 'boolean', description: 'does tools/backend-parity exist + cargo-check clean in the main tree?' },
    driftCount: { type: 'number', description: 'number of fixtures that regressed vs the committed baseline (drift exit-code != 0 cases)' },
    taskCount: { type: 'number', description: 'number of distinct port-tasks (drift + migrate-plan), after dedup' },
    tasks: { type: 'array', items: PORT_TASK_SCHEMA },
    driftCmd: { type: 'string', description: 'the exact drift command that was run' },
    planCmd: { type: 'string', description: 'the exact migrate-plan command that was run' },
    rawSummary: { type: 'string', description: 'the trailing harness output proving the counts (PARITY/DIFF lines, drift exit, JSON head)' },
  },
}

const MIGRATE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['id', 'target', 'filesTouched', 'changeSummary', 'sliceParity', 'sliceOutput', 'openConcerns'],
  properties: {
    id: { type: 'string' },
    target: { type: 'string' },
    filesTouched: { type: 'array', description: 'absolute paths of Rust files created/edited', items: { type: 'string' } },
    changeSummary: { type: 'string', description: 'what was implemented on the lagging side to match OXC' },
    sliceCmd: { type: 'string', description: 'the exact harness slice command re-run for this task' },
    sliceParity: { type: 'boolean', description: 'true only if the harness slice for THIS task now reports byte-identical output (no DIFF) for its fixtures' },
    sliceOutput: { type: 'string', description: 'trailing harness output proving the slice pass/diff' },
    openConcerns: { type: 'string', description: 'anything still diverging for this task and why, or empty string' },
  },
}

const VERIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['oneToOne', 'parityRan', 'driftClean', 'diffFixtures', 'summary'],
  properties: {
    oneToOne: { type: 'boolean', description: 'true only if every enabled backend emits byte-identical output across the WHOLE corpus AND drift is clean' },
    parityRan: { type: 'boolean', description: 'did `backend-parity parity` actually run over the corpus?' },
    driftClean: { type: 'boolean', description: 'did `backend-parity drift` exit 0 (no regression vs baseline)?' },
    parityCmd: { type: 'string' },
    diffFixtures: { type: 'array', description: 'fixture ids still emitting non-byte-identical output, with a one-line reason each', items: { type: 'string' } },
    summary: { type: 'string', description: 'the trailing harness output proving the verdict + which backends were enabled' },
  },
}

// ---------------------------------------------------------------------------
// Phase 1 — Detect: run the harness, parse the drift set + the port-tasks.
// ---------------------------------------------------------------------------

phase('Detect')

log(`backend-parity: detecting drift/port-tasks via the harness at ${MANIFEST}`)
log(`targets to keep 1:1 with OXC: ${TARGETS.join(', ')}; maxTasks=${MAX_TASKS}`)

const detect = await agent(
  `You are the DETECT phase of Treaty's backend-parity migration engine. Treaty is a Rust/OXC Angular (Ivy) compiler whose DEFAULT backend is OXC; the lagging backends that must stay byte-identical to OXC are the SWC backend and the React-emit target (per ${ROOT}/migration/SWC-BACKEND-PLAN.md §3.3 and §4). The deterministic NO-AI harness lives at ${ROOT}/tools/backend-parity (its own detached [workspace], mirroring tools/render3-sync).

Run the harness from the repo root (${ROOT}) and parse its JSON output. Run, in order:
  1. \`${CMD_DRIFT}\`   (the plan spells this \`${RUN_P} drift\`)
  2. \`${CMD_MIGRATE_PLAN}\`   (the plan spells this \`${RUN_P} migrate-plan\`)

The \`drift\` subcommand re-runs the corpus through every ENABLED backend and diffs against the committed baseline (tools/backend-parity/baseline.json); it EXITS NON-ZERO when a fixture regressed (byte-different output) — capture every regressed fixture id. The \`migrate-plan\` subcommand emits STRUCTURED JSON port-tasks describing what each lagging backend (swc / react) must implement to match OXC, seeded from the embedded oxc→swc / oxc→react API table (cross-reference ${ROOT}/migration/SWC-BACKEND-PLAN.md §2).

NOTES / FALLBACKS:
- If \`-p ${PKG}\` fails because the crate is a DETACHED workspace, use the \`--manifest-path ${MANIFEST}\` form shown above (that is the robust spelling from the root).
- If tools/backend-parity does NOT exist in the main tree yet (it may have been built in a worktree by the backend-parity-harness workflow and not cherry-picked), set ran=false, harnessExists=false, and emit an empty tasks array with rawSummary explaining the harness is absent. Do NOT invent tasks.
- The output may be JSON on stdout or a PARITY <id> OK|DIFF table — parse whichever the harness emits. Do NOT verify emitted/compiled code with regex; read the harness's own structured output.

Build the unified task list:
- Each REGRESSED fixture from \`drift\` becomes a port-task with source="drift" (the lagging side must be re-aligned to the OXC baseline for that fixture).
- Each entry from \`migrate-plan\` becomes a port-task with source="migrate-plan".
- For each task, set a stable kebab \`id\`, the \`target\` backend (one of: ${TARGETS.join(', ')}), affected \`fixtures\`, and the expected \`files\`. Prefer the harness's own ids; if it gives none, derive a deterministic id from the fixture/target (do NOT use timestamps or random — derive from the stable fixture/target names).
- Dedup by id. Cap at ${MAX_TASKS} tasks (keep the drift regressions first, then migrate-plan tasks in the harness's order).

Set driftCount = number of regressed fixtures, taskCount = number of tasks after dedup, ran = whether the harness actually executed over the corpus on the oxc side. Put the trailing harness output proving the counts in rawSummary. Return via the StructuredOutput tool.`,
  { label: 'detect', phase: 'Detect', schema: DETECT_SCHEMA }
)

const allTasks = (detect && Array.isArray(detect.tasks)) ? detect.tasks : []
// Keep only tasks for targets we are migrating; dedup by id (stable, index-ordered).
const seen = new Set()
const tasks = []
for (let i = 0; i < allTasks.length && tasks.length < MAX_TASKS; i++) {
  const t = allTasks[i]
  if (!t || !t.id) continue
  if (t.target && !TARGETS.includes(t.target)) continue
  if (seen.has(t.id)) continue
  seen.add(t.id)
  tasks.push(t)
}

log(`detect: ran=${detect && detect.ran}; drift=${detect && detect.driftCount}; ${tasks.length} port-task(s) after filter/dedup`)

// ---------------------------------------------------------------------------
// Phase 2 — Migrate: one worktree-isolated agent per port-task implements the
// lagging-side change and re-runs the harness for its slice.
// ---------------------------------------------------------------------------

phase('Migrate')

function migratePrompt(task, idx) {
  const info = TARGET_INFO[task.target] || TARGET_INFO.swc
  const fixtureList = (Array.isArray(task.fixtures) && task.fixtures.length)
    ? task.fixtures.join(',')
    : ''
  // The harness slice filter for THIS task. Vary the slice by the task id (stable),
  // NOT by time/random. Prefer fixture ids if the harness supports --only on them.
  const sliceArg = fixtureList ? `--only ${fixtureList}` : `--only ${task.id}`
  const sliceCmd = `${CMD_PARITY} ${sliceArg}`
  const expectedFiles = (Array.isArray(task.files) && task.files.length) ? task.files : info.files

  return `You are MIGRATE agent #${idx + 1} for Treaty's backend-parity engine. Your job: implement ONE port-task on the LAGGING ${info.title} so it emits byte-identical output to the leading OXC backend, then prove it via the harness slice.

You are running inside an ISOLATED git worktree — edit freely; touch ONLY the files this task needs. Other tasks run in parallel in their own worktrees, so do NOT edit shared scaffolding outside your slice.

PORT-TASK (from ${task.source || 'migrate-plan'}):
${JSON.stringify(task, null, 2)}

TARGET: ${info.title} — ${info.blurb}.
  Likely Rust files:  ${expectedFiles.join('\n                      ')}
  Plan reference:     ${info.plan}
  The 1:1 invariant:  the SAME input must emit BYTE-IDENTICAL Ivy (or matched React output) under OXC and ${task.target}. OXC is the reference — match it; never change the OXC output to make the diff go away.

TASK:
1. Read the relevant Rust on the lagging side (${expectedFiles.join(', ')}) and the matching OXC implementation it must mirror (the OXC Lowerer / parse path in libs/treaty-ivy). Read the plan section for this task's oxc→${task.feature || task.target} API mapping.
2. Implement the change the port-task describes — the missing instruction lowering, the diverging formatting (precedence-based parenthesization, quote/whitespace/number-literal style), or the parse/metadata read — so the ${info.title} output matches OXC for this task's fixtures.
3. Re-run the harness for YOUR SLICE ONLY (from ${ROOT}):
     ${sliceCmd}
   (If the harness has no \`--only\` flag, run the full \`${CMD_PARITY}\` and report just your fixtures' lines.) Set sliceParity=true ONLY if your fixtures now report byte-identical output (no DIFF). Capture the trailing harness output in sliceOutput and the exact command in sliceCmd.
4. If the lagging backend is behind a Cargo feature (e.g. \`--no-default-features --features ${task.feature || task.target}\`), build/run it with that feature so your change is actually exercised.

RULES:
- Do NOT verify emitted/compiled code with regex (false positives). Run the harness or print + read the real emitted output.
- Leave ZERO marker words (TODO / FIXME / todo!() / NOTE(port)) in code you touch.
- Do NOT change the OXC baseline or the corpus fixtures to hide a diff. The OXC side is the source of truth.
- Edit ONLY the lagging-side files for this task; do not regress other fixtures.
- If you cannot fully close the diff, implement as much as is correct, set sliceParity=false, and put the precise remaining divergence (fixture + why) in openConcerns.

Return what you changed via the StructuredOutput tool (id="${task.id}", target="${task.target}").`
}

let migrations = []
if (tasks.length === 0) {
  log('migrate: no drift / open port-tasks — backends already 1:1, skipping fan-out')
} else {
  migrations = (await parallel(tasks.map((task, idx) => () =>
    agent(migratePrompt(task, idx), {
      label: `migrate:${(task.id || `task-${idx}`).slice(0, 28)}`,
      phase: 'Migrate',
      schema: MIGRATE_SCHEMA,
      isolation: 'worktree',
    })
  ))).filter(Boolean)
  const closed = migrations.filter(m => m && m.sliceParity).length
  log(`migrate: ${migrations.length} task(s) attempted; ${closed} slice(s) now byte-identical`)
}

// ---------------------------------------------------------------------------
// Phase 3 — Verify: re-run the whole-corpus parity gate and report 1:1 status.
// ---------------------------------------------------------------------------

phase('Verify')

const migrationDigest = migrations.length
  ? migrations.map((m, i) =>
      `  ${i + 1}. [${m && m.target}] ${m && m.id}: sliceParity=${m && m.sliceParity}` +
      (m && m.openConcerns ? ` — open: ${m.openConcerns}` : '')).join('\n')
  : '  (no migrations this run)'

const verify = await agent(
  `You are the VERIFY phase of Treaty's backend-parity engine. After the migrate phase implemented lagging-side changes (in isolated worktrees), confirm whether every backend is byte-identical 1:1 with the leading OXC backend AGAIN, over the WHOLE corpus.

Migrate-phase results this run:
${migrationDigest}

Run, from the repo root (${ROOT}):
  1. \`${CMD_PARITY}\`   (the plan spells this \`${RUN_P} parity\`) — runs every ENABLED backend over the entire corpus and asserts pairwise byte-equality of the emitted output per fixture; it prints a per-fixture PARITY <id> OK|DIFF table and exits non-zero on ANY diff.
  2. \`${CMD_DRIFT}\` — confirm no regression vs the committed baseline (exits 0 when clean).

If the lagging backends are behind Cargo features, also build them so they are actually exercised in the comparison (e.g. \`cargo build --manifest-path ${MANIFEST} --no-default-features --features swc\`), then run parity with the feature enabled. If \`-p ${PKG}\` fails because the crate is a DETACHED workspace, use the \`--manifest-path ${MANIFEST}\` form.

Set:
  - parityRan = whether \`parity\` actually ran over the corpus.
  - driftClean = whether \`drift\` exited 0 (no regression).
  - diffFixtures = every fixture still emitting non-byte-identical output, each with a one-line reason (which backend diverged + how: formatting / missing instruction / parse-metadata).
  - oneToOne = true ONLY if parityRan AND driftClean AND diffFixtures is empty (the backends are 1:1).
  - summary = the trailing harness output proving the verdict + which backends were enabled (e.g. oxc only, or oxc+swc).

NOTES:
- If tools/backend-parity is absent from the main tree (built in a worktree, not cherry-picked), say so plainly: set parityRan=false and explain in summary (the orchestrator must land the harness + the worktree migrations first).
- Do NOT verify emitted/compiled code with regex. Read the harness's own structured PARITY/DIFF output.
- Do NOT edit any source in this phase — verification only.

Return via the StructuredOutput tool.`,
  { label: 'verify', phase: 'Verify', schema: VERIFY_SCHEMA }
)

log(`verify: oneToOne=${verify && verify.oneToOne}; parityRan=${verify && verify.parityRan}; driftClean=${verify && verify.driftClean}`)

return {
  config: { manifest: MANIFEST, pkg: PKG, targets: TARGETS, maxTasks: MAX_TASKS },
  detect: {
    ran: detect && detect.ran,
    harnessExists: detect && detect.harnessExists,
    driftCount: detect && detect.driftCount,
    taskCount: tasks.length,
    driftCmd: detect && detect.driftCmd,
    planCmd: detect && detect.planCmd,
  },
  tasks,
  migrations,
  verify,
  oneToOne: !!(verify && verify.oneToOne),
}
