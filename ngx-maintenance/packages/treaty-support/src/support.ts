import type {
  TreatyAuthoringMode,
  TreatyMigrationStep,
  TreatyPackageOptions,
  TreatyShell,
  TreatyStepInput,
  TreatyStepResult,
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

/** The deterministic argv that applies one structural Treaty transform. */
function transformArgv(transform: string): readonly string[] {
  return ["treaty", "transform", transform];
}

/**
 * Build the PRODUCTION {@link TreatyMigrationStep}: it shells the planned
 * structural transforms (for `enhanced` mode) and then the treaty-packagr build
 * through the injected {@link TreatyShell}, in the cloned working directory.
 * Deterministic, no AI — the plan is a fixed function of the mode. The step
 * stops at (and reports) the FIRST non-zero command, so a human reviews any
 * failure rather than an LLM papering over it.
 *
 * The same shell the github-adapter already owns is injected here, so the
 * Treaty step runs through one process boundary and stays fully fake-testable.
 */
export function createTreatyMigrationStep(
  shell: TreatyShell,
): TreatyMigrationStep {
  return {
    async migrate(input: TreatyStepInput): Promise<TreatyStepResult> {
      const plan = planTreatyMigration(input.mode);

      for (const transform of plan.transforms) {
        const argv = transformArgv(transform);
        // eslint-disable-next-line no-await-in-loop
        const result = await shell(input.workdir, argv);
        if (result.code !== 0) {
          return { ok: false, plan, failedArgv: argv, output: result.output };
        }
      }

      const buildArgv = treatyPackagrArgv({
        projectRoot: ".",
        outDir: input.outDir,
        mode: input.mode,
      });
      const build = await shell(input.workdir, buildArgv);
      if (build.code !== 0) {
        return { ok: false, plan, failedArgv: buildArgv, output: build.output };
      }

      return { ok: true, plan, failedArgv: undefined, output: undefined };
    },
  };
}
