/*
 * api-breakage-scout
 * ------------------
 * Regenerates the OXC migration crib (migration/OXC-MIGRATION-CRIB.md shape) for the
 * NEXT oxc bump. Fans out one READ-ONLY Explore agent per Rust source-module GROUP that
 * touches the oxc_* APIs (apps/rust/authoring jsx + sfc + plugin, libs/treaty-ivy/{core,
 * template,decorators,facade}/src, libs/authoring/node bindings, and the supporting tools
 * that parse/emit via oxc). Each agent returns a schema-validated inventory of every oxc
 * symbol the group uses + the current call shape + the required replacement under the
 * pinned/target OXC version. A barrier (Synthesize phase) merges every module report into
 * one migration crib: crate/import changes, AstBuilder renames, removed ::new constructors,
 * enum/field changes, semantic Scoping changes, and per-module hot-spots — the same
 * sections as migration/OXC-MIGRATION-CRIB.md.
 *
 * Treaty is a Rust/OXC Angular compiler. The Ivy compiler is `treaty_ivy` (4 crates under
 * libs/treaty-ivy/{core,template,decorators,facade}); libs/render3 no longer exists. OXC is
 * pinned at 0.133. Build orchestrator is Moon (not Nx).
 *
 * INVOKE (defaults run with no args — current 0.133, target probe = latest):
 *   Workflow({ name: "api-breakage-scout" })
 *
 * INVOKE for a specific bump (e.g. moving 0.133 -> 0.140, Angular ref 22):
 *   Workflow({ name: "api-breakage-scout", args: { fromOxc: "0.133.0", toOxc: "0.140.0", angularRef: "22" } })
 */

export const meta = {
  name: 'api-breakage-scout',
  description: 'Fan-out read-only Explore agents (one per oxc-using Rust module group) to inventory every oxc_* symbol + its required replacement, then merge into a single OXC migration crib for the next oxc bump',
  phases: [
    { title: 'Inventory' },
    { title: 'Synthesize' },
    { title: 'Verify' },
  ],
}

const ROOT = 'd:/dev/treaty'
const CRIB = 'd:/dev/treaty/migration/OXC-MIGRATION-CRIB.md'
const ANGULAR_REF = 'd:/dev/treaty/tools/angular-ref'

// Pinned/target OXC + Angular. Defaults make the script runnable with zero args.
const fromOxc = (args && args.fromOxc) || '0.133.0'
const toOxc = (args && args.toOxc) || 'latest'
const angularRef = (args && args.angularRef) || '22'

// One Moon target probe per phase (Moon replaced Nx). Kept as data, not executed here —
// the agents read source; the orchestrator surfaces the commands a human/CI would run.
const MOON_BUILD = 'moon run :build'
const MOON_TEST = 'moon run :test'

// ---------------------------------------------------------------------------
// The oxc-using Rust module GROUPS to fan out over. Each entry = one Explore agent.
// Paths are real on disk (verified): apps/rust/authoring/src/{jsx,sfc,plugin,angular_source},
// libs/treaty-ivy/{core,template,decorators,facade}/src, libs/authoring/node, plus tools.
// ---------------------------------------------------------------------------
const MODULE_GROUPS = [
  {
    key: 'authoring-jsx',
    crate: 'authoring (apps/rust/authoring)',
    paths: [
      'apps/rust/authoring/src/jsx/mod.rs',
      'apps/rust/authoring/src/jsx/template.rs',
      'apps/rust/authoring/src/jsx/signals.rs',
      'apps/rust/authoring/src/jsx/react.rs',
      'apps/rust/authoring/src/jsx/ts_erase.rs',
      'apps/rust/authoring/src/jsx/control_flow.rs',
      'apps/rust/authoring/src/jsx/angular_blocks.rs',
      'apps/rust/authoring/src/jsx/directives.rs',
    ],
    focus: 'JSX -> Angular authoring lowering: AstBuilder node construction, VisitMut transforms, TS erasure, control-flow + signals rewriting. Heaviest AstBuilder/Atom/Argument user.',
  },
  {
    key: 'authoring-sfc-plugin',
    crate: 'authoring (apps/rust/authoring)',
    paths: [
      'apps/rust/authoring/src/sfc.rs',
      'apps/rust/authoring/src/angular_source.rs',
      'apps/rust/authoring/src/plugin/mod.rs',
      'apps/rust/authoring/src/plugin/ts_to_rust.rs',
    ],
    focus: '.treaty SFC parse + standard @Component Angular-source ingestion + backend plugin TS->Rust shim. Parser/SemanticBuilder entry points + SourceType.',
  },
  {
    key: 'ivy-core',
    crate: 'treaty_ivy core (libs/treaty-ivy/core)',
    paths: [
      'libs/treaty-ivy/core/src/output_ast.rs',
      'libs/treaty-ivy/core/src/output/mod.rs',
      'libs/treaty-ivy/core/src/output/emitter.rs',
      'libs/treaty-ivy/core/src/output/source_map.rs',
      'libs/treaty-ivy/core/src/expression/lexer.rs',
      'libs/treaty-ivy/core/src/expression/parser.rs',
      'libs/treaty-ivy/core/src/expression/ast.rs',
      'libs/treaty-ivy/core/src/expression_converter.rs',
      'libs/treaty-ivy/core/src/factory.rs',
      'libs/treaty-ivy/core/src/identifiers.rs',
      'libs/treaty-ivy/core/src/util.rs',
    ],
    focus: 'Ivy output_ast -> oxc AST emit, expression lexer/parser, factory + identifiers. Core AstBuilder/expression_* emitter surface and the printed-JS path.',
  },
  {
    key: 'ivy-template',
    crate: 'treaty_ivy template (libs/treaty-ivy/template)',
    paths: [
      'libs/treaty-ivy/template/src/ml_parser.rs',
      'libs/treaty-ivy/template/src/binder.rs',
      'libs/treaty-ivy/template/src/template/mod.rs',
      'libs/treaty-ivy/template/src/template/control_flow.rs',
      'libs/treaty-ivy/template/src/template/deferred.rs',
      'libs/treaty-ivy/template/src/template/template_transform.rs',
      'libs/treaty-ivy/template/src/view/mod.rs',
      'libs/treaty-ivy/template/src/view/template.rs',
      'libs/treaty-ivy/template/src/view/queries.rs',
      'libs/treaty-ivy/template/src/i18n.rs',
    ],
    focus: 'Template parse + t2 binder + view/instruction generation. Span/SPAN, expression AST bridging into oxc, control-flow + deferred lowering.',
  },
  {
    key: 'ivy-decorators',
    crate: 'treaty_ivy decorators (libs/treaty-ivy/decorators)',
    paths: [
      'libs/treaty-ivy/decorators/src/registry.rs',
      'libs/treaty-ivy/decorators/src/compiler.rs',
      'libs/treaty-ivy/decorators/src/pipe_module_injector.rs',
      'libs/treaty-ivy/decorators/src/shadow_css.rs',
    ],
    focus: 'DecoratorCompiler registry + @Component/@Directive/@Pipe/@NgModule/@Injectable codegen. PropertyKey, object_property, class property/element construction.',
  },
  {
    key: 'ivy-facade',
    crate: 'treaty_ivy facade (libs/treaty-ivy/facade)',
    paths: [
      'libs/treaty-ivy/facade/src/lib.rs',
      'libs/treaty-ivy/facade/src/compile.rs',
      'libs/treaty-ivy/facade/src/source_compile.rs',
      'libs/treaty-ivy/facade/src/linker.rs',
    ],
    focus: 'Public compile facade + the Angular partial-linker (ɵɵngDeclare* -> full Ivy). Parser/Semantic entry, program walk, re-emit. The linker is the newest oxc consumer.',
  },
  {
    key: 'authoring-node-bindings',
    crate: 'authoring node (libs/authoring/node)',
    paths: [
      'libs/authoring/node/src/lib.rs',
      'libs/authoring/node/build.rs',
      'libs/authoring/treaty_authoring/src/lib.rs',
    ],
    focus: 'NAPI bindings + the treaty_authoring re-export surface. Any oxc types that cross the NAPI boundary or appear in public signatures.',
  },
  {
    key: 'supporting-tools',
    crate: 'tools + runtime + packagr',
    paths: [
      'libs/packagr/src/fesm.rs',
      'libs/packagr/src/dts.rs',
      'libs/packagr/src/component_dts.rs',
      'libs/runtime/src/transpile.rs',
      'libs/runtime/src/node/resolver.rs',
      'tools/render3-sync/src/ts.rs',
      'tools/render3-sync/src/ts2rust.rs',
      'tools/render3-sync/src/drift.rs',
      'tools/dep-updater/src/model.rs',
      'tools/dep-updater/src/codemod.rs',
      'tools/dep-updater/src/orchestrate.rs',
    ],
    focus: 'packagr FESM/dts emit, runtime transpile + oxc_resolver, render3-sync ts->rust codegen, dep-updater codemods. Lower-volume but version-sensitive oxc consumers.',
  },
]

// One oxc symbol-usage record (what the agent reports per oxc symbol it finds in its group).
const SYMBOL_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['symbol', 'crate', 'kind', 'currentUsage', 'status', 'replacement', 'site'],
  properties: {
    symbol: { type: 'string', description: 'the oxc symbol/path as used today, e.g. "oxc_span::Atom", "AstBuilder::new_vec", "Argument::Expression", "SemanticBuilder::new(src)"' },
    crate: { type: 'string', description: 'owning oxc crate: oxc_ast | oxc_ast_visit | oxc_span | oxc_str | oxc_syntax | oxc_semantic | oxc_parser | oxc_allocator | oxc_codegen | oxc_resolver | oxc_traverse | other' },
    kind: { type: 'string', enum: ['import', 'type', 'astbuilder-method', 'constructor', 'enum-variant', 'field', 'entry-point', 'macro', 'trait', 'other'] },
    currentUsage: { type: 'string', description: 'the exact call/usage shape in the code today (signature or expression form), so the replacement is unambiguous' },
    status: { type: 'string', enum: ['UNCHANGED', 'RENAMED', 'SIGNATURE_CHANGED', 'MOVED_CRATE', 'REMOVED', 'TYPE_CHANGED', 'UNKNOWN'], description: 'how it changes under the target OXC version vs the pinned one' },
    replacement: { type: 'string', description: 'the required replacement call/import under the target OXC version; empty string if UNCHANGED. Include the new signature/arg-order when SIGNATURE_CHANGED.' },
    site: { type: 'string', description: 'file path (+ symbol/fn) where this is used; relative to repo root is fine' },
  },
}

const MODULE_REPORT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['groupKey', 'crate', 'depChanges', 'symbols', 'hotspots', 'notes'],
  properties: {
    groupKey: { type: 'string', description: 'the module-group key this report covers' },
    crate: { type: 'string', description: 'the owning Treaty crate/area' },
    depChanges: {
      type: 'array',
      description: 'Cargo.toml dep additions/removals/version moves implied by this group (e.g. add oxc_str, add oxc_ast_visit, bump oxc)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['dep', 'action', 'detail'],
        properties: {
          dep: { type: 'string' },
          action: { type: 'string', enum: ['ADD', 'REMOVE', 'BUMP', 'UNCHANGED'] },
          detail: { type: 'string' },
        },
      },
    },
    symbols: {
      type: 'array',
      description: 'every distinct oxc symbol this group uses + its required replacement under the target version',
      items: SYMBOL_SCHEMA,
    },
    hotspots: {
      type: 'array',
      description: 'the files/functions that will take the most edits, ordered most-breaking first',
      items: { type: 'string' },
    },
    notes: { type: 'string', description: 'group-specific gotchas, project-specific shim decisions, or anything that does not fit a symbol row' },
  },
}

const CRIB_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['fromVersion', 'toVersion', 'crateImportChanges', 'stringChanges', 'astBuilderRenames', 'removedConstructors', 'enumFieldChanges', 'semanticChanges', 'entryPoints', 'perModuleHotspots', 'projectDecisions'],
  properties: {
    fromVersion: { type: 'string' },
    toVersion: { type: 'string' },
    crateImportChanges: {
      type: 'array',
      description: 'crate/import-level changes (new deps, moved modules, removed re-exports) — mirrors the "Crate / import changes" section of the crib',
      items: { type: 'string' },
    },
    stringChanges: {
      type: 'array',
      description: 'string/identifier type changes (Atom/Ident/Str/CompactStr and builder.atom/ident/str) — mirrors the "Strings" section',
      items: { type: 'string' },
    },
    astBuilderRenames: {
      type: 'array',
      description: 'AstBuilder method renames / signature changes — mirrors the "AstBuilder method renames" table',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['old', 'new'],
        properties: {
          old: { type: 'string', description: 'old call shape in current code' },
          new: { type: 'string', description: 'new call shape under target version (note arg-order swaps, dropped/added params, span-first, the expression_*/statement_* variant)' },
        },
      },
    },
    removedConstructors: {
      type: 'array',
      description: '::new constructors removed in favor of AstBuilder — mirrors "Removed ::new constructors"',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['old', 'new'],
        properties: { old: { type: 'string' }, new: { type: 'string' } },
      },
    },
    enumFieldChanges: {
      type: 'array',
      description: 'enum flattening / field moves / struct->enum changes — mirrors "Enum / field changes"',
      items: { type: 'string' },
    },
    semanticChanges: {
      type: 'array',
      description: 'oxc_semantic Scoping merge + generate_uid removal etc — mirrors "oxc_semantic Scoping merge"',
      items: { type: 'string' },
    },
    entryPoints: {
      type: 'array',
      description: 'Parser / SemanticBuilder / codegen entry-point signature changes — mirrors "Parser / Semantic entry points"',
      items: { type: 'string' },
    },
    perModuleHotspots: {
      type: 'array',
      description: 'per-module-group migration hot-spots: which crate/files take the most edits and why',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['groupKey', 'crate', 'editCount', 'summary'],
        properties: {
          groupKey: { type: 'string' },
          crate: { type: 'string' },
          editCount: { type: 'number', description: 'count of breaking symbol-sites (RENAMED+SIGNATURE_CHANGED+MOVED_CRATE+REMOVED+TYPE_CHANGED) in this group' },
          summary: { type: 'string' },
        },
      },
    },
    projectDecisions: {
      type: 'array',
      description: 'project-specific decisions/shims needed (mirrors the "Project-specific decisions" section)',
      items: { type: 'string' },
    },
  },
}

const VERIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['coverageGaps', 'contradictions', 'missingSections', 'confidence', 'notes'],
  properties: {
    coverageGaps: {
      type: 'array',
      description: 'oxc-using files/symbols on disk that NO module-group agent reported (the fan-out missed them)',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['where', 'symbol', 'why'],
        properties: {
          where: { type: 'string' },
          symbol: { type: 'string' },
          why: { type: 'string' },
        },
      },
    },
    contradictions: {
      type: 'array',
      description: 'two agents that disagree on the same symbols replacement, or a replacement that contradicts the installed crate source',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['symbol', 'conflict'],
        properties: { symbol: { type: 'string' }, conflict: { type: 'string' } },
      },
    },
    missingSections: {
      type: 'array',
      description: 'sections the merged crib is missing vs the canonical crib shape (e.g. no Scoping section but oxc_semantic is used)',
      items: { type: 'string' },
    },
    confidence: { type: 'string', enum: ['HIGH', 'MEDIUM', 'LOW'], description: 'overall confidence the crib is complete + correct enough to drive the next bump' },
    notes: { type: 'string' },
  },
}

// ===========================================================================
phase('Inventory')

function inventoryPrompt(group, idx) {
  // Vary depth-of-scan by array index instead of any clock/random builtin: the first
  // (heaviest) groups get the deepest "also chase transitive helpers" instruction.
  const deepScan = idx < 3
    ? 'This is a HIGH-VOLUME oxc consumer: also chase helper fns and macros that wrap oxc calls, not just direct `use oxc_*` lines.'
    : 'Report direct oxc usage; note (do not deeply chase) any indirection through shared helpers.'
  return `You are auditing ONE module group of the Treaty Rust/OXC Angular compiler at "${ROOT}" for an OXC version bump.

MODULE GROUP: ${group.key}  (crate area: ${group.crate})
FOCUS: ${group.focus}
FILES (read every one that exists; some lists are representative — also Glob the sibling dir for any *.rs you were not handed):
${group.paths.map(p => `  - ${p}`).join('\n')}

BUMP CONTEXT:
- Pinned/current OXC version in this repo: ${fromOxc}.
- Target OXC version for the NEXT bump: ${toOxc} (if "latest", treat as the newest oxc_* crate source available under the cargo registry / additional working dirs; the installed oxc_ast / oxc_syntax sources are mounted as additional working directories and ARE the ground truth).
- Angular reference is vendored at "${ANGULAR_REF}" (target Angular ${angularRef}); only consult it if an oxc symbol is used specifically to mirror an Angular API shape.
- The canonical existing crib (the OUTPUT shape to think in) is "${CRIB}". Read it to learn the categories, then produce fresh findings for THIS group — do NOT just copy it.

${deepScan}

RULES:
- READ-ONLY. Do NOT edit files. Do NOT run \`cargo build\`/\`cargo test\` or \`${MOON_BUILD}\`/\`${MOON_TEST}\` (slow + parallel contention). Determine replacements by READING: this repo's source for current usage, and the INSTALLED oxc crate sources (mounted as additional working dirs, e.g. oxc_ast/src/ast and oxc_syntax/src) for the target signatures.
- NEVER infer an API shape from regex alone — open the actual oxc crate source (struct def, AstBuilder method, enum variants) and read the real signature before declaring a replacement.
- The Ivy compiler is \`treaty_ivy\` (4 crates under libs/treaty-ivy/{core,template,decorators,facade}); \`libs/render3\` no longer exists — do not look for it.
- For EVERY distinct oxc symbol the group uses, emit one symbols[] row: the symbol, its owning crate, kind, the exact current usage shape, a status (UNCHANGED / RENAMED / SIGNATURE_CHANGED / MOVED_CRATE / REMOVED / TYPE_CHANGED / UNKNOWN), the required replacement (empty if UNCHANGED), and the site (file + fn).
- depChanges[]: Cargo.toml additions/removals/bumps this group implies (e.g. ADD oxc_str, ADD oxc_ast_visit, BUMP oxc). Read the relevant Cargo.toml to ground this.
- hotspots[]: files/functions that take the most edits, most-breaking first.
- If you genuinely cannot determine a replacement from the installed sources, status=UNKNOWN with a note — do not guess.

Return via the StructuredOutput tool. groupKey="${group.key}".`
}

const reports = (await parallel(MODULE_GROUPS.map((g, idx) => () =>
  agent(inventoryPrompt(g, idx), {
    label: `inv:${g.key}`,
    phase: 'Inventory',
    schema: MODULE_REPORT_SCHEMA,
    agentType: 'Explore',
  })
))).filter(Boolean)

const allSymbols = reports.flatMap(r => (r.symbols || []).map(s => ({ ...s, groupKey: r.groupKey })))
const breaking = allSymbols.filter(s => s.status && s.status !== 'UNCHANGED' && s.status !== 'UNKNOWN')
const unknowns = allSymbols.filter(s => s.status === 'UNKNOWN')

// Per-group breaking edit counts (deterministic rollup the synthesizer can lean on).
const editCounts = new Map()
for (const s of breaking) editCounts.set(s.groupKey, (editCounts.get(s.groupKey) || 0) + 1)

log(`inventory: ${reports.length}/${MODULE_GROUPS.length} groups reported, ${allSymbols.length} oxc symbol-sites, ${breaking.length} breaking, ${unknowns.length} UNKNOWN`)

// ===========================================================================
phase('Synthesize')

const editCountLines = MODULE_GROUPS
  .map(g => `  - ${g.key} (${g.crate}): ${editCounts.get(g.key) || 0} breaking sites`)
  .join('\n')

const synthesized = await agent(
  `You are merging per-module OXC inventories into ONE migration crib for the Treaty Rust/OXC Angular compiler, for the bump ${fromOxc} -> ${toOxc}.

The output MUST mirror the canonical crib at "${CRIB}" — same sections: crate/import changes, Strings, AstBuilder method renames/signature changes, Removed ::new constructors, Enum/field changes, oxc_semantic Scoping merge, Parser/Semantic entry points, and project-specific decisions — PLUS a per-module hot-spot rollup.

Here are the ${reports.length} module-group reports (each from a read-only Explore agent over one group of oxc-using Rust files):
${JSON.stringify(reports, null, 2)}

Deterministic per-group breaking-site rollup (use these numbers for perModuleHotspots.editCount; do not recompute):
${editCountLines}

DO THIS:
1. Deduplicate symbols across groups (the same oxc symbol shows up in many modules) into a single canonical replacement per symbol. If two groups disagree on a replacement, prefer the one that cites the installed oxc crate source; surface the disagreement in projectDecisions/notes.
2. Bucket each deduped change into the right crib section by its kind/crate (import vs string vs astbuilder-method vs constructor vs enum/field vs semantic vs entry-point).
3. Fill perModuleHotspots[] one row per module group, using the provided editCount and a one-line summary of what breaks there.
4. projectDecisions[]: carry forward + extend the project-specific shim decisions (e.g. a generate_uid shim, DI baseline) only where the reports justify them.
5. Set fromVersion="${fromOxc}", toVersion="${toOxc}".

You MAY read the installed oxc crate sources (mounted as additional working dirs) to break ties, but do NOT run cargo/moon. Return the merged crib via the StructuredOutput tool.`,
  { label: 'synthesize-crib', phase: 'Synthesize', schema: CRIB_SCHEMA, agentType: 'Explore' }
)

// ===========================================================================
phase('Verify')

const symbolIndex = allSymbols.map(s => `${s.symbol} [${s.status}] @ ${s.groupKey}`).join('\n')

const verification = await agent(
  `You are the completeness + correctness critic for a freshly regenerated OXC migration crib (bump ${fromOxc} -> ${toOxc}) for the Treaty Rust/OXC Angular compiler at "${ROOT}".

The fan-out produced these oxc symbol-sites (across ${reports.length} module groups):
${symbolIndex}

The merged crib that resulted:
${JSON.stringify(synthesized, null, 2)}

YOUR JOB — find what the fan-out + merge MISSED or got wrong:
1. coverageGaps[]: Glob/Grep the repo for \`use oxc\` (and bare \`oxc_\`/\`oxc::\` paths) across apps/rust/authoring, libs/treaty-ivy/*/src, libs/authoring, libs/packagr, libs/runtime, tools/* and find any oxc-using file or symbol that NO module-group agent reported. List each gap.
2. contradictions[]: any symbol where the crib's replacement contradicts the INSTALLED oxc crate source (mounted as additional working dirs — read the real struct/method/enum) or where two agents disagreed.
3. missingSections[]: sections the canonical crib at "${CRIB}" has that the merged crib lacks despite the symbols using that area (e.g. oxc_semantic is used but there's no Scoping section).
4. confidence: HIGH/MEDIUM/LOW that this crib is complete + correct enough to drive the next bump.

RULES: READ-ONLY. Do NOT run \`cargo\`/\`${MOON_BUILD}\`/\`${MOON_TEST}\`. NEVER verify an API shape via regex — open the real oxc crate source. Return via the StructuredOutput tool.`,
  { label: 'verify-crib', phase: 'Verify', schema: VERIFY_SCHEMA, agentType: 'Explore' }
)

return {
  fromOxc,
  toOxc,
  angularRef,
  moonTargets: { build: MOON_BUILD, test: MOON_TEST },
  groups: MODULE_GROUPS.length,
  groupsReported: reports.length,
  totalSymbolSites: allSymbols.length,
  breakingSites: breaking.length,
  unknownSites: unknowns.length,
  editCounts: Object.fromEntries(editCounts),
  crib: synthesized,
  verification,
}
