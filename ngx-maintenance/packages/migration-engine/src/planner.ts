import { codemodsForStep } from "./codemods.js";
import {
  IVY_TRANSITION_MAJOR,
  LATEST_ANGULAR,
  VE_WINDOW_END,
  VE_WINDOW_START,
  type AngularMajor,
  type MigrationPlan,
  type MigrationStep,
  type PeerBump,
  type StepKind,
  type VerifyCommand,
} from "./types.js";

/** The framework packages whose versions are pinned to the Angular major. */
const FRAMEWORK_PEERS: readonly string[] = [
  "@angular/core",
  "@angular/common",
  "@angular/compiler",
  "@angular/compiler-cli",
  "@angular/cli",
];

/**
 * Whether crossing from `from` to `to` traverses the View-Engine -> Ivy
 * boundary (the official ng-update step that flips the compiler). Kept for the
 * vN-1 -> vN transition that straddles the boundary.
 */
export function requiresVeToIvy(
  from: AngularMajor,
  to: AngularMajor,
): boolean {
  return from <= VE_WINDOW_START && to >= IVY_TRANSITION_MAJOR;
}

/**
 * Whether a library pinned at `current` may still ship View-Engine artifacts
 * and therefore needs an explicit VE->Ivy transition before the chain proceeds.
 * True for the v9..v12 window: Ivy is the default from v9 but View Engine was
 * not fully removed until v13, so a lib in that window can still be VE.
 */
export function inVeToIvyWindow(current: AngularMajor): boolean {
  return current >= IVY_TRANSITION_MAJOR && current <= VE_WINDOW_END;
}

/** The standard install + build + test verification run after each step. */
function defaultVerify(): readonly VerifyCommand[] {
  return [
    { label: "install", argv: ["npm", "install"] },
    { label: "build", argv: ["npm", "run", "build"] },
    { label: "test", argv: ["npm", "test"] },
  ];
}

/** The framework peer bumps that align every Angular peer to `major`. */
function peerBumpsFor(major: AngularMajor): readonly PeerBump[] {
  return FRAMEWORK_PEERS.map((pkg) => ({ pkg, range: `^${major}.0.0` }));
}

/** The deterministic ng-update argv for a single major transition. */
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

/** Attach the codemod ids whose hooks target this step. */
function withCodemods(step: MigrationStep): MigrationStep {
  return { ...step, codemods: codemodsForStep(step).map((hook) => hook.id) };
}

/** Build a single major-transition step from `from` to `to`. */
function buildStep(from: AngularMajor, to: AngularMajor): MigrationStep {
  const kind: StepKind = requiresVeToIvy(from, to) ? "ve-to-ivy" : "ng-update";
  return withCodemods({
    kind,
    from,
    to,
    ngUpdateArgv: ngUpdateArgv(from, to),
    peerBumps: peerBumpsFor(to),
    codemods: [],
    verify: defaultVerify(),
  });
}

/**
 * Build the dedicated View-Engine -> Ivy transition step for a library pinned
 * inside the v9..v12 window. This runs the official Ivy migration *in place* at
 * the current major (from == to) before the chain advances, converting any
 * remaining View-Engine artifacts (ng-package partial compilation, ngcc) to
 * Ivy so subsequent major bumps build cleanly.
 */
function buildVeToIvyStep(current: AngularMajor): MigrationStep {
  return withCodemods({
    kind: "ve-to-ivy",
    from: current,
    to: current,
    ngUpdateArgv: ngUpdateArgv(current, current),
    peerBumps: peerBumpsFor(current),
    codemods: [],
    verify: defaultVerify(),
  });
}

/**
 * Plan the migration chain from `currentMajor` up to `targetMajor`.
 *
 * The plan is deterministic and ordered:
 *  1. If the library is pinned in the v9..v12 View-Engine window, a dedicated
 *     VE->Ivy step runs first (in place, at the current major).
 *  2. Then one `ng-update` step per major transition (current+1 .. target),
 *     each driving the OFFICIAL Angular migration plus framework peer bumps and
 *     any curated codemods for that major. A transition that straddles the
 *     legacy v8->v9 boundary is itself a `ve-to-ivy` step.
 *
 * Returns an empty step list when the library is already at (or beyond) target
 * and outside the VE->Ivy window. NO AI is involved.
 */
export function planMigrationChain(
  currentMajor: AngularMajor,
  targetMajor: AngularMajor = LATEST_ANGULAR,
): MigrationPlan {
  const steps: MigrationStep[] = [];
  if (inVeToIvyWindow(currentMajor)) {
    steps.push(buildVeToIvyStep(currentMajor));
  }
  for (let to = currentMajor + 1; to <= targetMajor; to += 1) {
    steps.push(buildStep(to - 1, to));
  }
  return { from: currentMajor, to: targetMajor, steps };
}

/**
 * Backwards-compatible alias for {@link planMigrationChain}. Retained because
 * downstream packages (the bot's release handler) import `planChain`.
 */
export const planChain = planMigrationChain;
