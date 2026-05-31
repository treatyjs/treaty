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
