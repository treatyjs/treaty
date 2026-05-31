/**
 * @ngx-maintenance/migration-engine
 *
 * The deterministic migration CHAIN. Given a cloned repository at Angular vX,
 * plan and run vX -> ... -> latest where each step is the OFFICIAL Angular
 * migration (`ng update @angular/core@N @angular/cli@N --migrate-only
 * --from=N-1 --to=N`) plus framework peer bumps, the View-Engine -> Ivy
 * transition for the v9..v12 window, and a curated set of deterministic
 * codemod hooks for gaps the official schematics do not cover. Each step is
 * verified by install + build + test. NO AI is involved at any stage.
 */

export type {
  AngularMajor,
  MigrationStep,
  MigrationPlan,
  PeerBump,
  CodemodId,
  StepKind,
  VerifyCommand,
} from "./types.js";
export {
  LATEST_ANGULAR,
  IVY_TRANSITION_MAJOR,
  VE_WINDOW_START,
  VE_WINDOW_END,
} from "./types.js";

export {
  planMigrationChain,
  planChain,
  requiresVeToIvy,
  inVeToIvyWindow,
} from "./planner.js";

export type {
  CommandResult,
  Shell,
  StepPhase,
  StepLogEntry,
  StepResult,
  StepStatus,
  MigrationResult,
  ChainResult,
} from "./runner.js";
export { runMigration, runStep } from "./runner.js";

export type {
  CodemodHook,
  Codemod,
  CodemodEngine,
  CodemodApply,
} from "./codemods.js";
export { CURATED_CODEMODS, codemodsForStep, findCodemod } from "./codemods.js";
