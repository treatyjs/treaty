import type { AngularMajor, CodemodId, MigrationStep, StepKind } from "./types.js";

/**
 * The transform engine a codemod is implemented with. Both are deterministic
 * AST tools — NO AI is ever involved. `oxc` is preferred for whole-program,
 * high-throughput rewrites; `ts-morph` for surgical, type-aware edits.
 */
export type CodemodEngine = "oxc" | "ts-morph";

/**
 * The concrete transform a codemod runs against a working tree. The real
 * implementation (added alongside the oxc / ts-morph wiring) edits files under
 * `repoDir` in place and resolves once the rewrite is complete. It is declared
 * structurally here so the engine typechecks without pulling the transform
 * libraries into this package's dependency graph.
 */
export type CodemodApply = (repoDir: string) => Promise<void>;

/**
 * A curated deterministic codemod hook. Each codemod targets a specific known
 * breakage that the official `ng update` schematics do not handle, is keyed to
 * the major step it applies to, and carries the transform that fixes it.
 * Deterministic, no AI: each entry is a known fix for a known breakage.
 */
export interface CodemodHook {
  readonly id: CodemodId;
  readonly description: string;
  /** The major this codemod runs alongside (matched against the step's `to`). */
  readonly appliesToMajor: AngularMajor;
  /**
   * Restrict the codemod to specific step kinds. When omitted the codemod runs
   * for any step whose destination major matches `appliesToMajor`.
   */
  readonly appliesToKinds?: readonly StepKind[];
  /** The deterministic AST engine this codemod is implemented with. */
  readonly engine: CodemodEngine;
  /**
   * The transform applied to the working tree. Hooks without a wired transform
   * yet (`apply` undefined) are recorded in the plan but treated as no-ops by
   * the runner until their oxc / ts-morph implementation lands.
   */
  readonly apply?: CodemodApply;
}

/** Backwards-compatible alias for the curated codemod hook shape. */
export type Codemod = CodemodHook;

/**
 * The curated codemod hook registry. Deterministic, no AI: each entry is a
 * known fix for a known breakage in a specific Angular major transition. The
 * `apply` transforms are wired structurally to oxc / ts-morph and run by the
 * orchestrator after the official schematic for that step.
 */
export const CURATED_CODEMODS: readonly CodemodHook[] = [
  {
    id: "ng-package-ivy-partial",
    description: "Switch ng-package.json to Ivy partial compilation mode.",
    appliesToMajor: 9,
    appliesToKinds: ["ve-to-ivy"],
    engine: "ts-morph",
  },
  {
    id: "remove-ivy-flag",
    description:
      "Drop the now-default `enableIvy`/`angularCompilerOptions.enableIvy` flag.",
    appliesToMajor: 12,
    engine: "ts-morph",
  },
  {
    id: "ngcc-drop",
    description: "Remove ngcc postinstall hooks made obsolete after Ivy.",
    appliesToMajor: 13,
    engine: "oxc",
  },
  {
    id: "entry-components-drop",
    description: "Delete entryComponents arrays (no-op since Ivy).",
    appliesToMajor: 13,
    engine: "ts-morph",
  },
  {
    id: "typescript-peer-bump",
    description: "Align the typescript devDependency with the Angular major.",
    appliesToMajor: 16,
    engine: "oxc",
  },
  {
    id: "standalone-bootstrap",
    description: "Migrate NgModule bootstrap to standalone bootstrapApplication.",
    appliesToMajor: 19,
    engine: "ts-morph",
  },
];

/**
 * The curated codemod hooks that apply to a given step: their destination
 * major matches the step's `to`, and (when constrained) the step kind is in the
 * hook's `appliesToKinds`.
 */
export function codemodsForStep(step: MigrationStep): readonly CodemodHook[] {
  return CURATED_CODEMODS.filter((hook) => {
    if (hook.appliesToMajor !== step.to) return false;
    if (hook.appliesToKinds && !hook.appliesToKinds.includes(step.kind)) {
      return false;
    }
    return true;
  });
}

/** Look up a single codemod hook by id, if it is registered. */
export function findCodemod(id: CodemodId): CodemodHook | undefined {
  return CURATED_CODEMODS.find((hook) => hook.id === id);
}
