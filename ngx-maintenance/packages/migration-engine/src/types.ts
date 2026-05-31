/** A supported Angular major version. */
export type AngularMajor = number;

/** The latest Angular major the chain migrates toward. */
export const LATEST_ANGULAR = 22;

/**
 * The major at which Ivy became the default compiler. Libraries published from
 * this major onward are partial-Ivy by default.
 */
export const IVY_TRANSITION_MAJOR = 9;

/** The first major in the View-Engine window that still needs VE handling. */
export const VE_WINDOW_START = 8;

/**
 * The last major in which a View-Engine library can still exist and be
 * migrated to Ivy. From v13 onward View Engine was removed entirely and every
 * library is Ivy, so the VE->Ivy transition only matters for the v9..v12
 * window (a lib pinned at one of those majors may still ship VE artifacts).
 */
export const VE_WINDOW_END = 12;

/** Stable identifier of a curated deterministic codemod. */
export type CodemodId = string;

/** The kind of work a single migration step performs. */
export type StepKind =
  | "ng-update"
  | "ve-to-ivy"
  | "peer-bump"
  | "codemod";

/** A framework peer-dependency bump applied as part of a step. */
export interface PeerBump {
  /** The npm package whose version is being aligned to the Angular major. */
  readonly pkg: string;
  /** The semver range to pin the peer to (caret-pinned to the major). */
  readonly range: string;
}

/** A shell verification command run after applying a step. */
export interface VerifyCommand {
  readonly label: string;
  readonly argv: readonly string[];
}

/** A single step in the migration chain (e.g. migrate v11 -> v12). */
export interface MigrationStep {
  readonly kind: StepKind;
  /** The major this step migrates FROM. */
  readonly from: AngularMajor;
  /** The major this step migrates TO. */
  readonly to: AngularMajor;
  /** The deterministic ng-update argv (empty for pure codemod/peer steps). */
  readonly ngUpdateArgv: readonly string[];
  /** Framework peer dependencies bumped alongside this step. */
  readonly peerBumps: readonly PeerBump[];
  /** Curated codemods to apply if the official migration leaves a gap. */
  readonly codemods: readonly CodemodId[];
  /** Verification commands run after the step (install + build + test). */
  readonly verify: readonly VerifyCommand[];
}

/** A full plan: the ordered set of steps from current to latest. */
export interface MigrationPlan {
  readonly from: AngularMajor;
  readonly to: AngularMajor;
  readonly steps: readonly MigrationStep[];
}
