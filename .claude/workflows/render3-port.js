/*
 * render3-port — port/extend the treaty_ivy Ivy-compiler modules to golden-file
 * parity with the vendored Angular reference (tools/angular-ref), one module at a
 * time, in strict dependency order.
 *
 * Treaty is a Rust/OXC Angular compiler. The Ivy compiler "treaty_ivy" lives in 4
 * crates under libs/treaty-ivy/{core,template,decorators,facade}. The OLD libs/render3
 * no longer exists. OXC is pinned at 0.133, Angular reference at tools/angular-ref.
 * Build orchestration is Moon (Nx was removed) — `moon run :build` / `moon run :test`.
 *
 * For each module (in dependency order: output_ast -> expr_parser -> template_ast ->
 * binder -> emitter) the workflow runs a bounded loop-until-parity:
 *   Stage 1 (Port):   a worktree-isolated agent ports/extends the module's Rust code.
 *   Stage 2 (Test):   runs `cargo test -p <crate> <module-tests>` for golden parity.
 *   Stage 3 (Review): an adversarial diff reviewer audits the port against the
 *                     Angular reference and the test output.
 * The loop repeats a module until its parity tests pass or MAX_ROUNDS is hit.
 *
 * Invoke with:
 *   Workflow({ name: "render3-port" })
 * Or override the module list / pins via args (defaults are built in):
 *   Workflow({ name: "render3-port", args: {
 *     modules: ["output_ast","expr_parser","template_ast","binder","emitter"],
 *     maxRounds: 3,
 *     oxc: "0.133",
 *     angular: "22",
 *   }})
 */

export const meta = {
  name: 'render3-port',
  description: 'Port/extend treaty_ivy Ivy-compiler modules to golden-file parity with the vendored Angular reference, in dependency order, looping each module until its parity tests pass',
  phases: [
    { title: 'Plan' },
    { title: 'Port' },
    { title: 'Test' },
    { title: 'Review' },
    { title: 'Report' },
  ],
}

const ROOT = 'd:/dev/treaty'
const IVY = 'd:/dev/treaty/libs/treaty-ivy'
const ANGULAR_REF = 'd:/dev/treaty/tools/angular-ref'

// Pins (overridable via args). Built-in defaults so the script runs with no args.
const OXC = (args && args.oxc) || '0.133'
const ANGULAR = (args && args.angular) || '22'
const MAX_ROUNDS = (args && Number(args.maxRounds)) || 3

// Module registry: logical render3 module -> real treaty_ivy crate + file + test filter.
// `crate` is the cargo package name; `testFilter` is the substring passed to cargo test
// to select that module's golden-parity tests. Listed in DEPENDENCY ORDER.
const MODULE_REGISTRY = {
  output_ast: {
    crate: 'treaty_ivy_core',
    file: `${IVY}/core/src/output_ast.rs`,
    testFilter: 'output_ast',
    ref: `${ANGULAR_REF}/packages/compiler/src/output/output_ast.ts`,
    summary: 'Output AST: the language-neutral expression/statement IR every later stage emits into.',
  },
  expr_parser: {
    crate: 'treaty_ivy_core',
    file: `${IVY}/core/src/expression/parser.rs`,
    testFilter: 'expression',
    ref: `${ANGULAR_REF}/packages/compiler/src/expression_parser/parser.ts`,
    summary: 'Expression parser + lexer: Angular template-expression grammar -> expression AST.',
  },
  template_ast: {
    crate: 'treaty_ivy_template',
    file: `${IVY}/template/src/template/r3_ast.rs`,
    testFilter: 'template',
    ref: `${ANGULAR_REF}/packages/compiler/src/render3/r3_ast.ts`,
    summary: 'Template AST (r3_ast): parsed HTML template -> render3 node tree (elements, bindings, control flow).',
  },
  binder: {
    crate: 'treaty_ivy_template',
    file: `${IVY}/template/src/binder.rs`,
    testFilter: 'binder',
    ref: `${ANGULAR_REF}/packages/compiler/src/render3/view/t2_binder.ts`,
    summary: 'Binder (t2_binder): resolves template references, variables, and directive matches over the template AST.',
  },
  emitter: {
    crate: 'treaty_ivy_core',
    file: `${IVY}/core/src/output/emitter.rs`,
    testFilter: 'emitter',
    ref: `${ANGULAR_REF}/packages/compiler/src/output/abstract_emitter.ts`,
    summary: 'Instruction emitter: lowers the output AST into the final Ivy JS instruction stream.',
  },
}

const DEFAULT_MODULES = ['output_ast', 'expr_parser', 'template_ast', 'binder', 'emitter']

// Accept the module list via args, with a built-in default. Drop unknowns.
const requestedModules = (args && Array.isArray(args.modules) && args.modules.length)
  ? args.modules
  : DEFAULT_MODULES
const modules = requestedModules.filter(m => MODULE_REGISTRY[m])

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

const PORT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['module', 'filesTouched', 'changeSummary', 'newTests', 'openConcerns'],
  properties: {
    module: { type: 'string' },
    filesTouched: {
      type: 'array',
      description: 'absolute paths of Rust files created or edited',
      items: { type: 'string' },
    },
    changeSummary: { type: 'string', description: 'what was ported/extended this round and how it maps to the Angular reference' },
    newTests: {
      type: 'array',
      description: 'names of golden/parity tests added or extended this round',
      items: { type: 'string' },
    },
    openConcerns: { type: 'string', description: 'known divergences from the Angular reference still outstanding, or empty string' },
  },
}

const TEST_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['module', 'crate', 'command', 'passed', 'failed', 'parity', 'failingTests', 'output'],
  properties: {
    module: { type: 'string' },
    crate: { type: 'string' },
    command: { type: 'string', description: 'the exact cargo test command run' },
    passed: { type: 'number' },
    failed: { type: 'number' },
    parity: { type: 'boolean', description: 'true only if all selected module-tests pass (failed === 0 and at least one test ran)' },
    failingTests: {
      type: 'array',
      description: 'names of failing tests with a one-line reason each',
      items: { type: 'string' },
    },
    output: { type: 'string', description: 'the trailing portion of cargo output that proves the pass/fail counts' },
  },
}

const REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['module', 'verdict', 'divergences', 'mustFix', 'notes'],
  properties: {
    module: { type: 'string' },
    verdict: { type: 'string', enum: ['PARITY', 'CLOSE', 'DIVERGENT'] },
    divergences: {
      type: 'array',
      description: 'concrete behavioural differences between the Rust port and the Angular reference, each with a file:symbol citation',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['where', 'angularBehaviour', 'rustBehaviour', 'severity'],
        properties: {
          where: { type: 'string', description: 'file:symbol or test name on the Rust side' },
          angularBehaviour: { type: 'string' },
          rustBehaviour: { type: 'string' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
        },
      },
    },
    mustFix: {
      type: 'array',
      description: 'ordered, concrete instructions for the next port round to reach parity; empty if verdict is PARITY',
      items: { type: 'string' },
    },
    notes: { type: 'string' },
  },
}

const REPORT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['summary', 'recommendedNext'],
  properties: {
    summary: { type: 'string', description: 'overall state of the treaty_ivy port across all modules attempted' },
    recommendedNext: {
      type: 'array',
      description: 'ordered next actions for any module that did not reach parity',
      items: { type: 'string' },
    },
  },
}

// ---------------------------------------------------------------------------
// Prompt builders
// ---------------------------------------------------------------------------

function portPrompt(name, info, round, priorMustFix) {
  const guidance = priorMustFix && priorMustFix.length
    ? `This is round ${round + 1}. The previous round did NOT reach parity. The adversarial reviewer requires you to address these, in order:\n${priorMustFix.map((m, i) => `  ${i + 1}. ${m}`).join('\n')}\n`
    : `This is round ${round + 1} (first attempt at this module).\n`

  return `You are porting/extending a single module of the treaty_ivy Ivy compiler (a Rust/OXC reimplementation of the Angular compiler) to golden-file parity with the vendored Angular reference.

MODULE: ${name}
  ${info.summary}
  Rust target file:        ${info.file}
  Cargo crate:             ${info.crate}
  Angular reference file:  ${info.ref}
  Parity test filter:      ${info.testFilter}

Repo root: ${ROOT}. OXC is pinned at ${OXC}; Angular reference is version ${ANGULAR} under ${ANGULAR_REF}.
You are running inside an ISOLATED git worktree — edit freely; do not touch other modules.

${guidance}

TASK:
1. Read the Angular reference (${info.ref}) and the current Rust file (${info.file}) plus its sibling modules in the same crate as needed.
2. Port/extend the Rust so its observable behaviour matches the Angular reference for the "${info.testFilter}" surface. Match data shapes, ordering, and emitted-string formatting EXACTLY — golden-file tests compare bytes.
3. Add or extend Rust golden/parity tests (under the same crate) that pin the behaviour against the reference. Mirror the reference's own test fixtures where they exist in ${ANGULAR_REF}.
4. Do NOT introduce new external deps and do NOT change the OXC ${OXC} pin. Stay within crate ${info.crate}.

RULES:
- NEVER verify emitted/compiled code with regex (false positives). Read the AST or print + read the real output.
- The render3 compiler was RENAMED to treaty_ivy; there is no libs/render3. All edits live under ${IVY}.
- You may run `cargo build -p ${info.crate}` to check it compiles, but DO NOT run the full test suite here — Stage 2 owns testing.

Return what you changed via the StructuredOutput tool (module="${name}").`
}

function testPrompt(name, info) {
  const cmd = `cargo test -p ${info.crate} ${info.testFilter}`
  return `Run the golden-file PARITY tests for the treaty_ivy module "${name}" and report results faithfully.

From the repo root (${ROOT}), run exactly:
  ${cmd}

This selects the "${info.testFilter}" tests in crate ${info.crate} that compare the Rust port's output against the Angular reference golden files.

Capture the test summary. Set:
  - passed / failed = the cargo test result counts for the selected tests.
  - parity = true ONLY if failed === 0 AND at least one test actually ran.
  - failingTests = each failing test name with a one-line reason (assertion / golden mismatch / panic).
  - output = the trailing lines of cargo output that prove the counts.
  - command = the exact command you ran.

Do NOT edit any source. Do NOT run `moon run :test` for the whole repo — only the targeted cargo command above. Report via the StructuredOutput tool (module="${name}").`
}

function reviewPrompt(name, info, port, test) {
  return `You are an ADVERSARIAL diff reviewer for a port of the treaty_ivy module "${name}" toward golden parity with the Angular reference. Assume the port is wrong until proven otherwise.

MODULE: ${name} — ${info.summary}
  Rust file:               ${info.file}
  Angular reference file:  ${info.ref}
  Crate:                   ${info.crate}

PORTER'S CLAIMED CHANGES THIS ROUND:
${JSON.stringify(port, null, 2)}

TEST STAGE RESULT THIS ROUND:
${JSON.stringify(test, null, 2)}

YOUR JOB — hunt for behavioural divergence from Angular, not style:
1. Diff the Rust file against the Angular reference symbol-by-symbol. Look for: missing branches/cases, wrong node/field ordering, off-by-one spans, dropped flags, formatting/whitespace differences in emitted strings, enum variants the reference has that the port lacks, and edge cases (empty input, nested control flow, i18n, self-closing tags — whatever applies to ${name}).
2. Confirm the porter's "newTests" actually pin the reference behaviour and are not tautological (e.g. asserting the port against itself instead of against a reference golden).
3. If tests pass but you find an unverified-by-tests divergence, that is still a divergence — call it out.

RULES:
- NEVER verify emitted/compiled code with regex. Read the AST or the real printed output.
- Use Glob/Grep/Read against ${ROOT} and ${ANGULAR_REF}. Do NOT run cargo (Stage 2 owns the run; trust the TEST STAGE RESULT for pass/fail).
- verdict: PARITY only if tests pass AND you found no blocker/major divergence. CLOSE if tests pass but minor gaps remain. DIVERGENT if tests fail OR a blocker/major divergence exists.
- mustFix: concrete, ordered instructions the next port round can act on. Empty array only when verdict is PARITY.

Report via the StructuredOutput tool (module="${name}").`
}

// ---------------------------------------------------------------------------
// Phase 1: Plan — order the requested modules by the canonical dependency order.
// ---------------------------------------------------------------------------

phase('Plan')

// Re-order whatever was requested into canonical dependency order (drop dupes).
const orderedModules = DEFAULT_MODULES.filter(m => modules.includes(m))
log(`render3-port: ${orderedModules.length} module(s) in dependency order → ${orderedModules.join(' -> ')}`)
log(`pins: OXC ${OXC}, Angular ${ANGULAR}, maxRounds ${MAX_ROUNDS}`)

// ---------------------------------------------------------------------------
// Per-module pipeline, in dependency order. Each module runs a bounded
// loop-until-parity: Port (worktree) -> Test -> Review, repeated up to MAX_ROUNDS.
// ---------------------------------------------------------------------------

const moduleResults = []

for (let mi = 0; mi < orderedModules.length; mi++) {
  const name = orderedModules[mi]
  const info = MODULE_REGISTRY[name]

  // eslint-disable-next-line no-await-in-loop
  const result = await pipeline(`module:${name}`, async () => {
    let priorMustFix = []
    let reachedParity = false
    let lastPort = null
    let lastTest = null
    let lastReview = null
    const rounds = []

    for (let round = 0; round < MAX_ROUNDS; round++) {
      // Stage 1: Port / extend the module in an isolated worktree.
      // eslint-disable-next-line no-await-in-loop
      const port = await agent(portPrompt(name, info, round, priorMustFix), {
        label: `port:${name}#${round + 1}`,
        phase: 'Port',
        schema: PORT_SCHEMA,
        isolation: 'worktree',
      })
      lastPort = port

      // Stage 2: Run the targeted golden-parity cargo tests.
      // eslint-disable-next-line no-await-in-loop
      const test = await agent(testPrompt(name, info), {
        label: `test:${name}#${round + 1}`,
        phase: 'Test',
        schema: TEST_SCHEMA,
      })
      lastTest = test

      // Stage 3: Adversarial diff review vs the Angular reference.
      // eslint-disable-next-line no-await-in-loop
      const review = await agent(reviewPrompt(name, info, port, test), {
        label: `review:${name}#${round + 1}`,
        phase: 'Review',
        schema: REVIEW_SCHEMA,
        agentType: 'Explore',
      })
      lastReview = review

      rounds.push({ round: round + 1, port, test, review })

      const testParity = !!(test && test.parity && test.failed === 0)
      const reviewParity = !!(review && review.verdict === 'PARITY')
      if (testParity && reviewParity) {
        reachedParity = true
        log(`module ${name}: PARITY reached on round ${round + 1}`)
        break
      }

      // Feed the reviewer's blockers into the next port round.
      priorMustFix = (review && Array.isArray(review.mustFix)) ? review.mustFix : []
      log(`module ${name}: round ${round + 1} not at parity (testParity=${testParity}, verdict=${review && review.verdict}); ${priorMustFix.length} must-fix item(s) carried forward`)
    }

    return {
      module: name,
      crate: info.crate,
      reachedParity,
      rounds: rounds.length,
      finalVerdict: lastReview && lastReview.verdict,
      finalTest: lastTest,
      finalPort: lastPort,
      finalReview: lastReview,
    }
  })

  moduleResults.push(result)
}

// ---------------------------------------------------------------------------
// Phase 5: Report — synthesize the cross-module state and next actions.
// ---------------------------------------------------------------------------

phase('Report')

const atParity = moduleResults.filter(m => m && m.reachedParity).map(m => m.module)
const notAtParity = moduleResults.filter(m => m && !m.reachedParity)

const reportDigest = moduleResults.map(m =>
  `- ${m.module} (${m.crate}): ${m.reachedParity ? 'PARITY' : 'NOT-AT-PARITY'} after ${m.rounds} round(s); ` +
  `final verdict=${m.finalVerdict || 'n/a'}; tests passed=${m.finalTest && m.finalTest.passed}/failed=${m.finalTest && m.finalTest.failed}`
).join('\n')

const report = await agent(
  `You are summarizing a treaty_ivy port run. The workflow ported these modules in dependency order toward golden parity with the Angular reference (${ANGULAR_REF}, Angular ${ANGULAR}, OXC ${OXC}); each module looped Port -> Test -> Review up to ${MAX_ROUNDS} rounds.

Per-module outcomes:
${reportDigest}

Modules at parity: ${atParity.length ? atParity.join(', ') : '(none)'}
Modules NOT at parity: ${notAtParity.length ? notAtParity.map(m => m.module).join(', ') : '(none)'}

Write a concise overall summary and an ordered list of recommended next actions for the modules that did not reach parity (be concrete: which divergence to close first, in which file). Because the modules build on each other in dependency order (output_ast -> expr_parser -> template_ast -> binder -> emitter), call out if an early module's gap is likely blocking a later one. Report via the StructuredOutput tool.`,
  { label: 'port-report', phase: 'Report', schema: REPORT_SCHEMA, agentType: 'Explore' }
)

return {
  modulesAttempted: orderedModules,
  pins: { oxc: OXC, angular: ANGULAR, maxRounds: MAX_ROUNDS },
  atParity,
  notAtParity: notAtParity.map(m => m.module),
  moduleResults,
  report,
}
