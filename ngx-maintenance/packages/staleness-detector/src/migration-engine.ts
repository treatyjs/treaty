import type { AngularMajor } from "./types.js";

/** The latest Angular major the chain migrates toward by default. */
export const LATEST_ANGULAR = 22;

/**
 * The major at which Ivy became the default compiler. Libraries published from
 * this major onward are partial-Ivy by default.
 */
export const IVY_TRANSITION_MAJOR = 9;

/**
 * The last major in which a View-Engine library can still exist and be migrated
 * to Ivy. From v13 onward View Engine was removed entirely, so the explicit
 * VE->Ivy transition only matters for the v9..v12 window (a lib pinned at one of
 * those majors may still ship View-Engine artifacts).
 */
export const VE_WINDOW_END = 12;

/** What a single migration step does. */
export type MigrationStepKind = "ve-to-ivy" | "ng-update";

/**
 * One ordered step in the migration sequence. `ve-to-ivy` runs in place at the
 * current major (`from === to`) to flip remaining View-Engine artifacts to Ivy
 * before the chain advances; `ng-update` migrates one major (`to === from + 1`).
 */
export interface MigrationStep {
  readonly kind: MigrationStepKind;
  /** The major this step migrates FROM. */
  readonly from: AngularMajor;
  /** The major this step migrates TO. */
  readonly to: AngularMajor;
  /** The deterministic `ng update` argv the migration engine should run. */
  readonly ngUpdateArgv: readonly string[];
}

/**
 * Whether a library pinned at `current` may still ship View-Engine artifacts and
 * therefore needs an explicit VE->Ivy transition before the chain proceeds. True
 * for the v9..v12 window: Ivy is the default from v9 but View Engine was not
 * fully removed until v13.
 */
export function inVeToIvyWindow(current: AngularMajor): boolean {
  return current >= IVY_TRANSITION_MAJOR && current <= VE_WINDOW_END;
}

/** The deterministic `ng update` argv for a single major transition. */
function ngUpdateArgv(from: AngularMajor, to: AngularMajor): readonly string[] {
  return [
    "ng",
    "update",
    `@angular/core@${to}`,
    `@angular/cli@${to}`,
    "--migrate-only",
    `--from=${from}`,
    `--to=${to}`,
    "--allow-dirty",
  ];
}

/**
 * Sequence the ordered migration steps from `currentMajor` up to `targetMajor`.
 *
 * Deterministic and ordered:
 *  1. If the library is pinned in the v9..v12 View-Engine window, a dedicated
 *     VE->Ivy step runs FIRST (in place, at the current major).
 *  2. Then exactly one `ng-update` step per major transition (current+1 ..
 *     target), each driving the OFFICIAL Angular `ng update` migration.
 *
 * Returns an empty list when the library is already at (or beyond) target and
 * outside the VE->Ivy window. Drives the migration-engine package; NO AI.
 */
export function sequenceMigrationSteps(
  currentMajor: AngularMajor,
  targetMajor: AngularMajor = LATEST_ANGULAR,
): readonly MigrationStep[] {
  const steps: MigrationStep[] = [];
  if (inVeToIvyWindow(currentMajor)) {
    steps.push({
      kind: "ve-to-ivy",
      from: currentMajor,
      to: currentMajor,
      ngUpdateArgv: ngUpdateArgv(currentMajor, currentMajor),
    });
  }
  for (let to = currentMajor + 1; to <= targetMajor; to += 1) {
    steps.push({
      kind: "ng-update",
      from: to - 1,
      to,
      ngUpdateArgv: ngUpdateArgv(to - 1, to),
    });
  }
  return steps;
}

/** The ordered list of target majors a migration from `currentMajor` touches. */
export function migrationVersionPath(
  currentMajor: AngularMajor,
  targetMajor: AngularMajor = LATEST_ANGULAR,
): readonly AngularMajor[] {
  return sequenceMigrationSteps(currentMajor, targetMajor).map(
    (step) => step.to,
  );
}
