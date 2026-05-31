import type {
  TreatyAuthoringMode,
  TreatyPackageOptions,
  TreatySupportPlan,
} from "./types.js";

/** The default structural transforms applied when adopting Treaty authoring. */
const ENHANCED_TRANSFORMS: readonly string[] = [
  "treaty-decorator-rewrite",
  "treaty-template-binding",
  "treaty-package-manifest",
];

/**
 * Plan a fork's migration onto Treaty. The `enhanced` mode applies the full set
 * of authoring transforms; `compat` keeps Angular authoring and only switches
 * the packaging path. Deterministic — no AI.
 */
export function planTreatyMigration(
  mode: TreatyAuthoringMode,
): TreatySupportPlan {
  return {
    mode,
    usePackagr: true,
    transforms: mode === "enhanced" ? ENHANCED_TRANSFORMS : [],
  };
}

/** The deterministic treaty-packagr argv for building a fork. */
export function treatyPackagrArgv(
  options: TreatyPackageOptions,
): readonly string[] {
  return [
    "treaty-packagr",
    "--project",
    options.projectRoot,
    "--out",
    options.outDir,
    "--mode",
    options.mode,
  ];
}
