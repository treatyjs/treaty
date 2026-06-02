/*
 * render3-study — re-runnable Rust-port spec generator for treaty_ivy.
 *
 * WHAT IT DOES
 *   Fans out a pool of read-only Explore agents (one per module) over the vendored
 *   Angular reference compiler under tools/angular-ref/packages/compiler/src/{render3,
 *   output,expression_parser}. Each agent reads exactly ONE source module and emits a
 *   structured Rust-port spec — { module, inputs, outputs, dataStructures[],
 *   instructionMapping[], edgeCases[] } — mirroring the hand-written docs already in
 *   migration/render3-specs/. A final synthesis agent stitches the per-module specs into
 *   a single port index (dependency-ordered, with coverage gaps + treaty_ivy crate
 *   placement). This is the machine half of the tools/render3-sync harness: re-run it to
 *   refresh the port specs whenever the vendored Angular reference is bumped, then diff the
 *   result against treaty_ivy to find drift.
 *
 *   The Ivy compiler is `treaty_ivy` — 4 crates under libs/treaty-ivy/{core,template,
 *   decorators,facade}. `libs/render3` no longer exists. OXC is pinned at 0.133.
 *
 * HOW TO INVOKE
 *   Workflow({ name: "render3-study" })
 *
 *   Optional args (all have built-in defaults so it runs with zero args):
 *   Workflow({ name: "render3-study", args: {
 *     ref: "tools/angular-ref/packages/compiler/src",  // vendored Angular reference root
 *     specsDir: "migration/render3-specs",             // existing hand-written specs to mirror
 *     angularTarget: "22.1.0-next.0",                  // Angular reference version being mapped
 *     oxcTarget: "0.133",                              // pinned OXC version for the port
 *     build: false,                                    // if true, run `moon run :build`/`:test` gate first
 *   }})
 */

export const meta = {
  name: 'render3-study',
  description: 'Re-runnable Rust-port spec generator: one Explore agent per vendored Angular render3/output/expression_parser module emits a structured port spec, then synthesizes a dependency-ordered port index for keeping treaty_ivy in sync (tools/render3-sync).',
  phases: [
    { title: 'Survey' },
    { title: 'Spec' },
    { title: 'Index' },
  ],
}

const ROOT = 'd:/dev/treaty'

const args = (typeof globalThis !== 'undefined' && globalThis.args) || {}
const REF = args.ref || 'tools/angular-ref/packages/compiler/src'
const SPECS_DIR = args.specsDir || 'migration/render3-specs'
const ANGULAR_TARGET = args.angularTarget || '22.1.0-next.0'
const OXC_TARGET = args.oxcTarget || '0.133'
const RUN_BUILD = args.build === true

const REF_ABS = `${ROOT}/${REF}`
const SPECS_ABS = `${ROOT}/${SPECS_DIR}`

// Curated module set: one source file per Rust-port spec, in rough dependency order.
// `slug` -> matches the migration/render3-specs/NN-<slug>.md naming convention so each
// generated spec lines up with the hand-written reference (or fills a coverage gap).
const MODULES = [
  // output IR + emitters (the substrate every generator builds into)
  { slug: 'output_ast', path: 'output/output_ast.ts', area: 'output', role: 'language-agnostic output IR (o.*) every generator emits into' },
  { slug: 'abstract_emitter', path: 'output/abstract_emitter.ts', area: 'output', role: 'generic source-text + sourcemap emitter over the output IR' },
  { slug: 'abstract_js_emitter', path: 'output/abstract_js_emitter.ts', area: 'output', role: 'JS-flavored emitter specialization' },
  { slug: 'output_source_map', path: 'output/source_map.ts', area: 'output', role: 'v3 source-map builder for emitted code' },
  // expression parser (Angular template binding expressions)
  { slug: 'expr_lexer', path: 'expression_parser/lexer.ts', area: 'expression_parser', role: 'tokenizer for Angular binding expressions' },
  { slug: 'expr_ast', path: 'expression_parser/ast.ts', area: 'expression_parser', role: 'expression AST node hierarchy + visitors' },
  { slug: 'expr_parser', path: 'expression_parser/parser.ts', area: 'expression_parser', role: 'recursive-descent parser: tokens -> expression AST' },
  // render3 template parse + transform
  { slug: 'r3_ast', path: 'render3/r3_ast.ts', area: 'render3', role: 'render3 template AST (t-nodes) + visitor' },
  { slug: 'template_transform', path: 'render3/r3_template_transform.ts', area: 'render3', role: 'HTML AST -> render3 template AST transform' },
  { slug: 'control_flow', path: 'render3/r3_control_flow.ts', area: 'render3', role: '@if/@for/@switch control-flow block lowering' },
  { slug: 'deferred_blocks', path: 'render3/r3_deferred_blocks.ts', area: 'render3', role: '@defer block parsing + trigger wiring' },
  { slug: 'deferred_triggers', path: 'render3/r3_deferred_triggers.ts', area: 'render3', role: '@defer trigger parsing (on/when/hydrate)' },
  // render3 view compilers (instruction generation)
  { slug: 'view_compiler', path: 'render3/view/compiler.ts', area: 'render3/view', role: 'component/host metadata -> ɵɵdefineComponent + instruction emit' },
  { slug: 'view_template', path: 'render3/view/template.ts', area: 'render3/view', role: 'template AST -> create/update ɵɵ instruction streams' },
  { slug: 't2_binder', path: 'render3/view/t2_binder.ts', area: 'render3/view', role: 'template type-binder: scopes, references, used directives/pipes' },
  { slug: 'query_generation', path: 'render3/view/query_generation.ts', area: 'render3/view', role: 'ViewChild/ContentChild query instruction generation' },
  // render3 decorator/class compilers
  { slug: 'factory', path: 'render3/r3_factory.ts', area: 'render3', role: 'ɵɵngDeclareFactory / ɵfac factory function generation' },
  { slug: 'identifiers', path: 'render3/r3_identifiers.ts', area: 'render3', role: 'ExternalReference table of all ɵɵ runtime instruction identifiers' },
  { slug: 'pipe_compiler', path: 'render3/r3_pipe_compiler.ts', area: 'render3', role: 'ɵɵdefinePipe generation' },
  { slug: 'module_compiler', path: 'render3/r3_module_compiler.ts', area: 'render3', role: 'ɵɵdefineNgModule generation' },
  { slug: 'injector_compiler', path: 'render3/r3_injector_compiler.ts', area: 'render3', role: 'ɵɵdefineInjector generation' },
  { slug: 'class_metadata_compiler', path: 'render3/r3_class_metadata_compiler.ts', area: 'render3', role: 'ɵsetClassMetadata debug-metadata generation' },
]

// JSON-Schema for the per-module Rust-port spec each agent returns.
const SPEC_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['module', 'inputs', 'outputs', 'dataStructures', 'instructionMapping', 'edgeCases'],
  properties: {
    module: { type: 'string', description: 'the source module path relative to the compiler src root, e.g. render3/r3_factory.ts' },
    inputs: {
      type: 'array',
      description: 'what flows INTO this module: source IR/AST types, metadata structs, upstream compiler modules it imports/depends on',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['name', 'kind', 'from'],
        properties: {
          name: { type: 'string', description: 'the input type/struct/value name' },
          kind: { type: 'string', description: 'e.g. ast-node, metadata, ir, config, external-ref, host-ast' },
          from: { type: 'string', description: 'originating module/path or "caller-provided"' },
        },
      },
    },
    outputs: {
      type: 'array',
      description: 'what this module PRODUCES: output-AST expressions/statements, emitted instruction calls, transformed AST, parsed structures',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['name', 'kind', 'consumedBy'],
        properties: {
          name: { type: 'string', description: 'the produced type/value name' },
          kind: { type: 'string', description: 'e.g. output-ir, instruction-call, template-ast, parsed-ast, source-text' },
          consumedBy: { type: 'string', description: 'downstream module/path that consumes it, or "final-emit"' },
        },
      },
    },
    dataStructures: {
      type: 'array',
      description: 'the key classes/enums/interfaces in this module and their proposed Rust mapping (mirror the §3 tables in migration/render3-specs docs)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['tsName', 'tsShape', 'rustMapping'],
        properties: {
          tsName: { type: 'string', description: 'the TypeScript class/enum/interface name' },
          tsShape: { type: 'string', description: 'key fields/variants, one line (verbatim field names where load-bearing)' },
          rustMapping: { type: 'string', description: 'proposed idiomatic Rust type: enum variant, struct, Box<>/Vec<> children; note any OXC reuse or deliberate arena-free choice' },
        },
      },
    },
    instructionMapping: {
      type: 'array',
      description: 'the ɵɵ runtime instructions or output forms this module emits/maps (empty array if it emits no ɵɵ instructions, e.g. a pure-data or parser module)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['trigger', 'emits', 'notes'],
        properties: {
          trigger: { type: 'string', description: 'the source construct / AST node / metadata that triggers it' },
          emits: { type: 'string', description: 'the ɵɵ instruction or output-IR form produced, e.g. ɵɵelementStart, ɵɵproperty, ɵɵdefineComponent, o.InvokeFunctionExpr' },
          notes: { type: 'string', description: 'ordering/create-vs-update phase, args, slot allocation, or edge conditions' },
        },
      },
    },
    edgeCases: {
      type: 'array',
      description: 'gotchas, version-sensitive surface, intentional quirks to reproduce, and deferral candidates (mirror the §7 "edge cases" lists in the existing specs)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['case', 'impact'],
        properties: {
          case: { type: 'string', description: 'the gotcha / quirk / version-sensitive behavior' },
          impact: { type: 'string', description: 'why it matters for the Rust port and how to handle it (reproduce verbatim / guard / defer / stub)' },
        },
      },
    },
  },
}

// JSON-Schema for the final synthesized port index.
const INDEX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['portOrder', 'crateMap', 'coverageGaps', 'driftRisks', 'summary'],
  properties: {
    portOrder: {
      type: 'array',
      description: 'modules in dependency order (port-first to port-last), each with the modules it must be ported after',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['module', 'after', 'rationale'],
        properties: {
          module: { type: 'string' },
          after: { type: 'array', items: { type: 'string' }, description: 'module paths that must be ported before this one (empty if a leaf)' },
          rationale: { type: 'string' },
        },
      },
    },
    crateMap: {
      type: 'array',
      description: 'which treaty_ivy crate each module belongs in: libs/treaty-ivy/{core,template,decorators,facade}',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['module', 'crate', 'reason'],
        properties: {
          module: { type: 'string' },
          crate: { type: 'string', enum: ['core', 'template', 'decorators', 'facade', 'unknown'] },
          reason: { type: 'string' },
        },
      },
    },
    coverageGaps: {
      type: 'array',
      description: 'modules in the vendored reference that have NO matching hand-written spec in migration/render3-specs/, or specs that look stale vs the current vendored source',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['module', 'gap'],
        properties: {
          module: { type: 'string' },
          gap: { type: 'string', description: 'no existing spec / stale spec / partial — and what is missing' },
        },
      },
    },
    driftRisks: {
      type: 'array',
      description: 'highest-churn / most version-sensitive modules most likely to drift between Angular bumps; what to watch when re-running this generator',
      items: { type: 'string' },
    },
    summary: { type: 'string', description: 'one-paragraph synthesis: total modules specced, suggested first ports, biggest risks' },
  },
}

phase('Survey')

// Confirm the vendored reference + existing specs are present (vary nothing by clock/random;
// this is a pure existence read so the fan-out targets real files).
const survey = await agent(
  `You are surveying the vendored Angular reference compiler so a fan-out of port-spec agents only targets files that exist.

1. List the source files under "${REF_ABS}/render3", "${REF_ABS}/render3/view", "${REF_ABS}/output", and "${REF_ABS}/expression_parser" (use Glob — do NOT run cargo or moon).
2. List the existing hand-written specs under "${SPECS_ABS}" (these are the mirror target; the new specs must match their depth + shape).
3. For EACH of the following candidate modules, report whether the file exists at "${REF_ABS}/<path>":
${MODULES.map((m, i) => `   ${i + 1}. ${m.path}  (slug: ${m.slug})`).join('\n')}

Return a concise text report: which candidate paths exist, which are MISSING (so the orchestrator can skip them), and any obviously important render3/output/expression_parser source file that is NOT in the candidate list above (so coverage gaps are visible). Do not write any files.`,
  { label: 'survey-ref', phase: 'Survey', agentType: 'Explore' }
)

log(`survey complete; fanning out ${MODULES.length} module port-spec agents`)

// Optional build/test gate via Moon (Nx was removed). Off by default so the script runs
// with zero args and never blocks the read-only spec generation on a slow compile.
if (RUN_BUILD) {
  log('build gate enabled: moon run :build then moon run :test')
  await bash('moon run :build', { cwd: ROOT })
  await bash('moon run :test', { cwd: ROOT })
}

phase('Spec')

function specPrompt(m, idx) {
  return `You are generating ONE Rust-port spec for the Treaty project's Ivy compiler (treaty_ivy). Treaty is a Rust/OXC Angular compiler; OXC is pinned at ${OXC_TARGET}; the Angular reference is vendored at version ${ANGULAR_TARGET}.

READ EXACTLY ONE SOURCE MODULE — do not read others except to resolve a type you must map:
  "${REF_ABS}/${m.path}"
This module's role: ${m.role} (area: ${m.area}).

MIRROR the existing hand-written specs so this one drops into the same series. Read the closest matching reference doc under "${SPECS_ABS}" first (e.g. the doc whose filename slug resembles "${m.slug}") to learn the expected depth, the §3 data-structure tables, and the §7 edge-case style. If no matching doc exists, this module is a COVERAGE GAP — still produce a full spec.

Produce a faithful port spec capturing:
- inputs: every type/IR/metadata/upstream module that flows in (with where it comes from).
- outputs: what it produces and which downstream module consumes it (or "final-emit").
- dataStructures: each key class/enum/interface with its verbatim TS shape (field/variant names) and a PROPOSED idiomatic Rust mapping — enum-per-leaf-class, Box<>/Vec<> children, bitflags for modifier flags. Prefer an arena-free IR (Box/Vec) and only reach for oxc_ast (${OXC_TARGET}) at the final emit boundary; call out any place oxc_ast / oxc_codegen / oxc_resolver should be reused vs ported by hand.
- instructionMapping: the ɵɵ runtime instructions or output-IR forms emitted (source construct -> ɵɵinstruction / o.* node), with create-vs-update phase and arg/slot notes. If this module emits NO ɵɵ instructions (pure data, lexer, parser, AST def), return an EMPTY array — do not invent instructions.
- edgeCases: version-sensitive surface, intentional quirks to reproduce verbatim, NaN/-0/null-vs-undefined traps, and anything that can be deferred/stubbed in the port.

RULES:
- Read source as ground truth. Do NOT verify emitted code with regex; if you need to confirm an instruction name, read render3/r3_identifiers.ts. Do NOT run cargo, moon, or any build.
- Be precise with names and field shapes — the tools/render3-sync drift checker diffs these against treaty_ivy.
- Return strictly via the StructuredOutput tool matching the schema. Set module="${m.path}".`
}

const specs = (await parallel(MODULES.map((m, idx) => () =>
  agent(specPrompt(m, idx), {
    label: `spec:${m.slug}`,
    phase: 'Spec',
    schema: SPEC_SCHEMA,
    agentType: 'Explore',
    isolation: 'worktree',
  })
))).filter(Boolean)

log(`generated ${specs.length}/${MODULES.length} module port specs`)

phase('Index')

const specDigest = specs.map(s => {
  const ds = (s.dataStructures || []).map(d => d.tsName).join(', ')
  const ins = (s.instructionMapping || []).length
  return `- ${s.module}: ${(s.dataStructures || []).length} data structures [${ds}]; ${ins} instruction mappings; ${(s.edgeCases || []).length} edge cases`
}).join('\n')

const moduleCrateHints = MODULES.map(m => `- ${m.path} (${m.area}): ${m.role}`).join('\n')

const index = await agent(
  `You are synthesizing a dependency-ordered Rust-port INDEX from per-module specs of the vendored Angular reference compiler, for the Treaty Ivy compiler (treaty_ivy). treaty_ivy is 4 crates under libs/treaty-ivy/{core,template,decorators,facade}; libs/render3 no longer exists; OXC is pinned at ${OXC_TARGET}; Angular reference is ${ANGULAR_TARGET}.

Per-module specs were just generated for these modules:
${specDigest}

Module roles (for crate placement):
${moduleCrateHints}

Survey of what exists in the vendored reference vs the hand-written specs:
${typeof survey === 'string' ? survey : JSON.stringify(survey)}

Produce the port index:
- portOrder: order the modules port-first to port-last by dependency. Leaves (e.g. output/output_ast.ts, expression_parser/ast.ts, expression_parser/lexer.ts, render3/r3_identifiers.ts) come first; instruction generators (render3/view/*) and decorator compilers come last. For each, list which modules must be ported before it.
- crateMap: place each module in core / template / decorators / facade. Heuristic: output IR + emitters + expression parser + r3_ast = core; template transform + control flow + defer + view compilers + t2_binder = template; r3_factory/pipe/module/injector/class-metadata compilers = decorators; the public compile/link entry points = facade. Justify each.
- coverageGaps: modules in the reference with NO matching migration/render3-specs doc, or specs that look stale vs current source (use the survey).
- driftRisks: the highest-churn / most version-sensitive modules to watch when re-running this generator after an Angular bump (e.g. control flow, defer triggers, identifiers table).
- summary: one paragraph.

You may Read migration/render3-specs/ and the vendored reference to confirm placement, but do NOT run cargo/moon. Return via the StructuredOutput tool.`,
  { label: 'port-index', phase: 'Index', schema: INDEX_SCHEMA, agentType: 'Explore' }
)

return {
  angularTarget: ANGULAR_TARGET,
  oxcTarget: OXC_TARGET,
  modulesRequested: MODULES.length,
  specsGenerated: specs.length,
  specs,
  index,
}
