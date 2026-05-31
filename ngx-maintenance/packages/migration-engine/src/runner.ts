import { findCodemod } from "./codemods.js";
import type { MigrationPlan, MigrationStep, PeerBump } from "./types.js";

/** Outcome of a single step (or the chain as a whole). */
export type StepStatus = "success" | "failed";

/** The result of one shelled-out command. */
export interface CommandResult {
  /** Process exit code; 0 means success. */
  readonly code: number;
  /** Combined stdout + stderr. */
  readonly output: string;
}

/**
 * A structural shell: runs one argv in `repoDir` and resolves with its exit
 * code + combined output. The concrete implementation (spawning `ng`, `npm`,
 * etc.) is injected so the orchestrator stays pure and unit-testable. NO AI.
 */
export type Shell = (
  repoDir: string,
  argv: readonly string[],
) => Promise<CommandResult>;

/** Which phase of a step a log line came from. */
export type StepPhase = "ng-update" | "peer-bump" | "codemod" | "verify";

/** A single recorded action within a step (one shelled command or codemod). */
export interface StepLogEntry {
  readonly phase: StepPhase;
  readonly label: string;
  readonly code: number;
  readonly output: string;
}

/** The result of running one migration step. */
export interface StepResult {
  readonly step: MigrationStep;
  readonly status: StepStatus;
  /** Per-action log entries, in execution order. */
  readonly entries: readonly StepLogEntry[];
  /** Combined human-readable log (all entries concatenated). */
  readonly log: string;
}

/** Aggregate result of attempting a whole chain against a working tree. */
export interface MigrationResult {
  readonly repoDir: string;
  readonly status: StepStatus;
  /** Results of every step that was attempted, in order. */
  readonly results: readonly StepResult[];
  /** The first step that failed, if any (the chain stops there). */
  readonly failedAt: MigrationStep | undefined;
}

/** Backwards-compatible alias for the aggregate chain result. */
export type ChainResult = MigrationResult;

/** Render a single peer bump as the deterministic `npm pkg set` argv. */
function peerBumpArgv(bump: PeerBump): readonly string[] {
  return ["npm", "pkg", "set", `dependencies.${bump.pkg}=${bump.range}`];
}

/** Build a concatenated human-readable log from per-action entries. */
function joinLog(entries: readonly StepLogEntry[]): string {
  return entries
    .map((entry) => `[${entry.phase}:${entry.label}] (exit ${entry.code})\n${entry.output}`)
    .join("\n\n");
}

/**
 * Run one migration step against `repoDir`:
 *  1. the official `ng update` schematic (if the step carries one),
 *  2. framework peer bumps,
 *  3. curated codemods for the step's known breakages,
 *  4. install + build + test verification.
 *
 * Stops at the first non-zero exit and returns a failed result with the logs
 * gathered so far. NO AI.
 */
export async function runStep(
  repoDir: string,
  step: MigrationStep,
  shell: Shell,
): Promise<StepResult> {
  const entries: StepLogEntry[] = [];
  const fail = (): StepResult => ({
    step,
    status: "failed",
    entries,
    log: joinLog(entries),
  });

  if (step.ngUpdateArgv.length > 0) {
    // eslint-disable-next-line no-await-in-loop
    const result = await shell(repoDir, step.ngUpdateArgv);
    entries.push({
      phase: "ng-update",
      label: `${step.from}->${step.to}`,
      code: result.code,
      output: result.output,
    });
    if (result.code !== 0) return fail();
  }

  for (const bump of step.peerBumps) {
    // Peer bumps are sequential: each rewrites the same package.json.
    // eslint-disable-next-line no-await-in-loop
    const result = await shell(repoDir, peerBumpArgv(bump));
    entries.push({
      phase: "peer-bump",
      label: bump.pkg,
      code: result.code,
      output: result.output,
    });
    if (result.code !== 0) return fail();
  }

  for (const id of step.codemods) {
    const hook = findCodemod(id);
    if (hook?.apply) {
      // eslint-disable-next-line no-await-in-loop
      await hook.apply(repoDir);
    }
    entries.push({
      phase: "codemod",
      label: id,
      code: 0,
      output: hook
        ? `applied ${hook.engine} codemod: ${hook.description}`
        : `unknown codemod id: ${id}`,
    });
  }

  for (const command of step.verify) {
    // eslint-disable-next-line no-await-in-loop
    const result = await shell(repoDir, command.argv);
    entries.push({
      phase: "verify",
      label: command.label,
      code: result.code,
      output: result.output,
    });
    if (result.code !== 0) return fail();
  }

  return { step, status: "success", entries, log: joinLog(entries) };
}

/**
 * Orchestrate a planned migration chain against the working tree at `repoDir`,
 * step by step. Each step shells the official schematic + peer bumps + codemods
 * then verifies with install + build + test. Stops at the first failing step
 * and reports exactly which step failed (a human reviews failures — never an
 * LLM). Returns a structured result: success, or failed-at-step with logs.
 */
export async function runMigration(
  repoDir: string,
  plan: MigrationPlan,
  shell: Shell,
): Promise<MigrationResult> {
  const results: StepResult[] = [];
  for (const step of plan.steps) {
    // Steps MUST run sequentially: each migrates the tree produced by the
    // previous one, so parallelising would corrupt the working tree.
    // eslint-disable-next-line no-await-in-loop
    const result = await runStep(repoDir, step, shell);
    results.push(result);
    if (result.status === "failed") {
      return { repoDir, status: "failed", results, failedAt: step };
    }
  }
  return { repoDir, status: "success", results, failedAt: undefined };
}
