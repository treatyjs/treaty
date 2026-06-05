/*
 * ivy-parity-verify — the re-runnable Treaty Ivy parity GATE.
 *
 * What it does (the gate the migration plan wanted GREEN before any cutover off the
 * old TypeScript oracle path):
 *
 *   Phase 1 (Harness): actually RUN the two real harnesses that already live in the
 *   repo, in order, and PARSE their numbers — no fabrication:
 *     1. `cargo test -p treaty_ivy corpus_dump -- --ignored --nocapture`
 *        with TREATY_IVY_CORPUS_DUMP=<abs json path> set, which dumps a freshly-built
 *        treaty_ivy's `compile_component_source` output for every compliance case to JSON.
 *     2. `node libs/treaty-ivy/facade/compliance/run-compliance.mjs --cargo-dump=<path> --report`
 *        which fragment-matches that dump against Angular's own vendored compliance
 *        goldens and prints  Total / Compiled (runnable) / PASS / DIFF / Skipped and the
 *        Pass-rate (of runnable). The harness exits 0 and writes COMPLIANCE-REPORT.md
 *        (which also lists the sample DIFF cases + their gap category + `near:` detail).
 *     The Verify agent parses PASS / DIFF / runnable / rate AND the full list of DIFF
 *     case ids (with category + detail) out of the harness stdout + the report markdown.
 *
 *   Phase 2 (Classify): fan out ONE agent per reported DIFF case to classify it as a
 *   genuine COMPILER GAP (a treaty_ivy emit defect to fix) vs a BY-DESIGN artifact of
 *   the harness's golden-mode/canonicalizer (e.g. ng_modules JIT-mode goldens, the
 *   @let-spread canonicalizer fold, forwardRef-in-imports thunk — golden-authoring or
 *   match-mode limits, not compiler defects). Each returns {case, kind:'gap'|'by-design', why}.
 *
 * Returns: { pass, diff, runnable, rate, gaps[] } — gaps[] = the genuine-gap subset.
 *
 * Build orchestrator is Moon (Nx was removed). If the corpus dump cannot be produced
 * because treaty_ivy is stale, the Verify agent rebuilds via `moon run :build` /
 * `moon run :test` (NOT `nx run-many`) before re-running the dump.
 *
 * Invoke (no args needed — sensible built-in defaults):
 *   Workflow({ name: "ivy-parity-verify" })
 * Or pin the toolchain targets the gate asserts against:
 *   Workflow({ name: "ivy-parity-verify", args: { oxc: "0.133", angular: "22" } })
 */

export const meta = {
  name: 'ivy-parity-verify',
  description: 'Re-runnable Treaty Ivy parity GATE: run the real corpus-dump + run-compliance harnesses, parse PASS/DIFF/runnable/rate, then classify every reported DIFF as a genuine compiler gap vs a by-design golden-mode/shape artifact. Returns {pass,diff,runnable,rate,gaps[]}.',
  phases: [
    { title: 'Harness', detail: 'run cargo test -p treaty_ivy corpus_dump -- --ignored (TREATY_IVY_CORPUS_DUMP set), then node run-compliance.mjs --cargo-dump=<path> --report; parse PASS/DIFF/runnable/rate + the full DIFF case list (category + near-detail) from stdout + COMPLIANCE-REPORT.md' },
    { title: 'Classify', detail: 'fan out one agent per reported DIFF case: genuine compiler gap (treaty_ivy emit defect) vs by-design golden-mode/canonicalizer artifact; return {case, kind, why}' },
  ],
}

// ---------------------------------------------------------------------------
// Paths + toolchain targets. `args` is the workflow-invocation arg object; we read
// it defensively with built-in defaults so the gate runs with NO args.
// ---------------------------------------------------------------------------
const ROOT = 'd:/dev/treaty'
const IVY = 'd:/dev/treaty/libs/treaty-ivy'
const COMPLIANCE = 'd:/dev/treaty/libs/treaty-ivy/facade/compliance/run-compliance.mjs'
const REPORT = 'd:/dev/treaty/libs/treaty-ivy/facade/compliance/COMPLIANCE-REPORT.md'
// Deterministic dump path (no clock/random — those throw in workflow scripts).
const DUMP = 'd:/tmp/treaty-ivy-corpus-dump.json'

const OXC = (typeof args === 'object' && args && args.oxc) ? String(args.oxc) : '0.133'
const ANGULAR = (typeof args === 'object' && args && args.angular) ? String(args.angular) : '22'

// ---------------------------------------------------------------------------
// Schemas.
// ---------------------------------------------------------------------------

// Phase 1: the parsed harness result. `diffCases` is the authoritative list the
// Classify fan-out iterates over (one entry per runnable case that DIFFs).
const HARNESS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['ran', 'total', 'runnable', 'pass', 'diff', 'skipped', 'rate', 'dumpPath', 'diffCases', 'evidence'],
  properties: {
    ran: { type: 'boolean', description: 'true iff BOTH the cargo corpus_dump and run-compliance.mjs actually executed (not assumed)' },
    total: { type: 'number', description: 'Total compliance cases reported by the harness' },
    runnable: { type: 'number', description: 'Compiled (runnable) count' },
    pass: { type: 'number', description: 'PASS count' },
    diff: { type: 'number', description: 'DIFF count' },
    skipped: { type: 'number', description: 'Skipped (un-runnable) count' },
    rate: { type: 'string', description: 'Pass-rate of the runnable subset exactly as printed, e.g. "97.8%"' },
    dumpPath: { type: 'string', description: 'absolute path of the JSON corpus dump that was produced + consumed' },
    diffCases: {
      type: 'array',
      description: 'EVERY runnable case that DIFFs, parsed from harness --verbose stdout and/or COMPLIANCE-REPORT.md (the "Sample diverging cases" section). One entry per case.',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'category', 'detail'],
        properties: {
          id: { type: 'string', description: 'full case id, e.g. "r3_compiler_compliance/ng_modules/should define an NgModule ... (jit mode)"' },
          category: { type: 'string', description: 'the harness gap category (instruction/shape at first missing fragment), e.g. "misc-shape", "ɵɵcontrol", "def-header-counts"' },
          detail: { type: 'string', description: 'the `near:` divergence snippet the harness reported for this case (empty string if none)' },
        },
      },
    },
    evidence: { type: 'string', description: 'the exact commands run + the headline lines parsed from stdout (proof the numbers are real, not invented)' },
  },
}

// Phase 2: per-DIFF-case classification.
const CLASSIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['case', 'kind', 'why'],
  properties: {
    case: { type: 'string', description: 'the DIFF case id being classified' },
    kind: { type: 'string', enum: ['gap', 'by-design'], description: "'gap' = a genuine treaty_ivy emit defect to fix; 'by-design' = a golden-mode/canonicalizer/harness artifact, NOT a compiler defect" },
    why: { type: 'string', description: 'concrete justification citing the golden file, the emitted block, the canonicalizer rule, or the harness selection logic that was read — never a guess' },
  },
}

// ---------------------------------------------------------------------------
// Phase 1 — Harness: run both real harnesses and parse the numbers.
// ---------------------------------------------------------------------------
phase('Harness')

const harnessPrompt = `You are the PARITY GATE harness runner for the Treaty Ivy compiler (treaty_ivy = 4 crates under libs/treaty-ivy/{core,template,decorators,facade}; OXC pinned at ${OXC}, Angular reference at tools/angular-ref vendored for v${ANGULAR}). Your job is to ACTUALLY RUN the two existing harnesses and PARSE their real output. Do NOT invent numbers.

Working dir is the repo root "${ROOT}".

STEP 1 — produce the corpus dump (a freshly-built treaty_ivy's compile_component_source over every compliance case). Run EXACTLY (PowerShell: set the env var inline, do NOT use bash \`VAR=x cmd\` syntax):
   $env:TREATY_IVY_CORPUS_DUMP = "${DUMP}"; cargo test -p treaty_ivy corpus_dump -- --ignored --nocapture
   - The test is named \`corpus_dump::dump_corpus\`, #[ignore]'d, and gated on the TREATY_IVY_CORPUS_DUMP env var — it writes the JSON map {"<corpus-rel-input>":{code,errors}} to that path. It runs a NORMAL \`cargo test\` (no addon rebuild).
   - If cargo reports the crate is stale / fails to build, rebuild via Moon (NOT nx run-many — Nx was removed): \`moon run :build\` then retry. Use \`moon run :test\` only if a plain rebuild is insufficient.
   - Confirm the dump file exists at "${DUMP}" and is non-empty before continuing.

STEP 2 — score the dump against Angular's own goldens. Run EXACTLY:
   node ${COMPLIANCE} --cargo-dump=${DUMP} --report --verbose
   - This consumes the dump (NOT the live NAPI addon — so the score reflects the freshly-built treaty_ivy without rebuilding authoring_node), fragment-matches every case against the vendored compliance goldens, PRINTS the headline block, writes "${REPORT}", and exits 0.
   - The headline stdout lines are literally:
       Total compliance cases     : <N>
       Compiled (runnable)        : <N>
         PASS  : <N>
         DIFF  : <N>
       Skipped (un-runnable)      : <N>
       Pass-rate (of runnable)    : <X>%  (<pass>/<runnable>)
   - With --verbose each diverging case also prints two lines:  "DIFF <id>"  then  "   <category>: ...<detail>...".

STEP 3 — PARSE and assemble the result. Read the printed headline lines for total/runnable/pass/diff/skipped/rate. For the DIFF case list, collect EVERY diverging case: cross-check the \`DIFF <id>\` lines from --verbose stdout against the "## Ranked gap categories" + "### Sample diverging cases" sections of "${REPORT}" (which give category + the \`near:\` snippet). There must be exactly \`diff\` entries in diffCases — if --verbose missed any, open the report to recover the rest. Capture the category and near-detail per case.

HARD RULES:
- Do NOT guess or hand-compute numbers — they MUST come from the harness stdout / report you actually produced this run.
- Do NOT edit any Rust crate, the harness, or the goldens. This is a READ/RUN-ONLY gate.
- Do NOT verify emitted code with regex against goldens yourself; the harness already does the canonicalize+fragment match — your job is to RUN it and parse its verdict.
- Set ran=true ONLY if both commands actually executed and you parsed real output; otherwise ran=false and put the failure in evidence.
- dumpPath = "${DUMP}". evidence = the exact two commands + the verbatim headline lines.

Return via the StructuredOutput tool.`

const harness = await agent(harnessPrompt, { label: 'run-harnesses', phase: 'Harness', schema: HARNESS_SCHEMA })

const diffCases = (harness && Array.isArray(harness.diffCases)) ? harness.diffCases.filter(Boolean) : []
log(`harness: runnable=${harness ? harness.runnable : '?'} pass=${harness ? harness.pass : '?'} diff=${harness ? harness.diff : '?'} rate=${harness ? harness.rate : '?'} → ${diffCases.length} DIFF case(s) to classify`)

// ---------------------------------------------------------------------------
// Phase 2 — Classify: one agent per reported DIFF case (gap vs by-design).
// ---------------------------------------------------------------------------
phase('Classify')

function classifyPrompt(c, idx) {
  return `You are classifying ONE diverging compliance case from the Treaty Ivy parity gate as a genuine COMPILER GAP vs a BY-DESIGN golden-mode/canonicalizer/harness artifact. Inspect the REAL files at "${ROOT}" — do not trust the category label alone.

DIFF CASE #${idx + 1}:
${JSON.stringify(c, null, 2)}

The harness is "${COMPLIANCE}" (read its canonicalize() + matchGolden() + pickFullGolden()/selectMatchingBlock() logic and the file header doc). The case's input + golden live under tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/<the category path in the id>; the freshly-built emit for this case is the corresponding entry in the dump "${harness ? harness.dumpPath : DUMP}" (keyed by the corpus-relative input path).

Determine the true cause of the divergence:
- kind='gap'  → treaty_ivy emits genuinely WRONG or MISSING Ivy for a shape it claims to support (a real emit defect: wrong/absent instruction, bad slot index, missing def field, etc.). These are the items a follow-up fix workflow must close before cutover.
- kind='by-design' → the divergence is NOT a compiler defect but a known harness/golden-mode limit. Per the migration notes, the canonical by-design buckets are:
    * ng_modules JIT-mode goldens (the golden is a JIT-mode \`(jit mode)\` shape the AOT source front-end does not — and should not — reproduce),
    * the @let-spread canonicalizer fold (local-name spelling the canonicalizer cannot equate),
    * forwardRef-in-imports thunk goldens,
    * any other golden-authoring / match-mode artifact where OUR emit is functionally correct Ivy but the fragment matcher cannot equate it.
  Only call by-design if you can point to the specific golden construct or canonicalizer/selection rule responsible.

METHOD (no regex verification of emitted code — read/parse, or compare the dump entry to the golden by eye/structure):
1. Read the case's golden .js define block and note what kind it anchors on + the diverging fragment near "${c.detail}".
2. Read this case's emit from the dump JSON entry; locate the same region.
3. Read the relevant canonicalize()/selection rule in the harness to see whether the difference is load-bearing (instruction/arg-order/slot — a gap) or purely a naming/golden-mode artifact the harness intentionally cannot fold (by-design).

Return {case:"${c.id}", kind, why} via the StructuredOutput tool. \`why\` MUST cite the concrete golden construct, emit slice, or harness rule you read.`
}

const classified = diffCases.length
  ? (await parallel(diffCases.map((c, idx) => () =>
      agent(classifyPrompt(c, idx), {
        label: `classify:${String(c.id || ('diff-' + idx)).slice(0, 40)}`,
        phase: 'Classify',
        schema: CLASSIFY_SCHEMA,
        agentType: 'Explore',
      })
    ))).filter(Boolean)
  : []

const gaps = classified.filter(c => c && c.kind === 'gap')
const byDesign = classified.filter(c => c && c.kind === 'by-design')
log(`classified ${classified.length} DIFF case(s): ${gaps.length} genuine gap(s), ${byDesign.length} by-design`)

// ---------------------------------------------------------------------------
// Gate verdict.
// ---------------------------------------------------------------------------
return {
  pass: harness ? harness.pass : 0,
  diff: harness ? harness.diff : 0,
  runnable: harness ? harness.runnable : 0,
  rate: harness ? harness.rate : '0.0%',
  gaps,
  // Full detail so the orchestrator can decide cutover-readiness.
  byDesign,
  total: harness ? harness.total : 0,
  skipped: harness ? harness.skipped : 0,
  dumpPath: harness ? harness.dumpPath : DUMP,
  harnessRan: harness ? !!harness.ran : false,
  // The gate is GREEN for cutover iff every reported DIFF is by-design (no genuine gaps left).
  gateGreen: !!(harness && harness.ran) && gaps.length === 0,
  evidence: harness ? harness.evidence : 'harness did not run',
}
