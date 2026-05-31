/** The Treaty authoring mode a fork adopts. */
export type TreatyAuthoringMode = "compat" | "enhanced";

/** Options passed to treaty-packagr when building a fork. */
export interface TreatyPackageOptions {
  /** Path to the library project root. */
  readonly projectRoot: string;
  /** Output directory for the packaged artifact. */
  readonly outDir: string;
  /** The authoring mode to build under. */
  readonly mode: TreatyAuthoringMode;
}

/** A plan describing how to move a fork onto Treaty authoring. */
export interface TreatySupportPlan {
  readonly mode: TreatyAuthoringMode;
  /** Whether the fork opts into Treaty packaging via treaty-packagr. */
  readonly usePackagr: boolean;
  /** Structural transform identifiers to apply during migration. */
  readonly transforms: readonly string[];
}

/**
 * The single process boundary the Treaty step shells commands through. It is
 * structurally identical to the github-adapter `Shell` so the orchestrator can
 * inject the SAME injected shell it already owns (real spawning shell in
 * production, recording fake in tests) without treaty-support depending on the
 * github-adapter package. Resolves with an exit code + combined output; never
 * rejects on a non-zero exit so callers always get a structured result.
 */
export type TreatyShell = (
  repoDir: string,
  argv: readonly string[],
) => Promise<{ readonly code: number; readonly output: string }>;

/** The input to one opt-in Treaty migration step against a cloned library. */
export interface TreatyStepInput {
  /** npm package name of the library being migrated onto Treaty. */
  readonly npmName: string;
  /** The checked-out working directory the transforms + build run in. */
  readonly workdir: string;
  /** The authoring mode to adopt (`compat` packaging-only, or `enhanced`). */
  readonly mode: TreatyAuthoringMode;
  /** Output directory passed to treaty-packagr (relative to `workdir`). */
  readonly outDir: string;
}

/** The deterministic outcome of one Treaty migration step. */
export interface TreatyStepResult {
  /** True when every transform AND the packagr build exited zero. */
  readonly ok: boolean;
  /** The plan that was executed (mode + transforms + packaging). */
  readonly plan: TreatySupportPlan;
  /** The argv of the first command that failed, when `ok` is false. */
  readonly failedArgv: readonly string[] | undefined;
  /** Combined output of the failing command, when `ok` is false. */
  readonly output: string | undefined;
}

/**
 * The injectable Treaty migration boundary. The orchestrator runs this as an
 * ADDITIONAL, opt-in step after a library's deterministic Angular migration
 * succeeds and before the PR opens — but ONLY for opted-in libraries. When a
 * library has not opted in, the orchestrator never constructs nor calls this,
 * so behaviour is unchanged. Fully fake-testable: tests inject a recording
 * implementation; production injects {@link createTreatyMigrationStep}.
 */
export interface TreatyMigrationStep {
  migrate(input: TreatyStepInput): Promise<TreatyStepResult>;
}
