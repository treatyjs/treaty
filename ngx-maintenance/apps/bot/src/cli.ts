#!/usr/bin/env node
import process from "node:process";
import { loadManifest } from "@ngx-maintenance/registry";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";
import type {
  BotConfigInput,
  MaintenanceCycleResult,
  TreatyAuthoringMode,
} from "@ngx-maintenance/orchestrator";
import {
  initialState,
  resolveSchedulerConfig,
} from "@ngx-maintenance/orchestrator";
import { createRunner } from "./runner.js";
import { poll } from "./poller.js";

/** The subcommands the runnable bot CLI accepts. */
export type CliCommand = "run-cycle" | "poll" | "scan";

/** Whether `command` is a recognised subcommand. */
export function isCliCommand(command: string): command is CliCommand {
  return command === "run-cycle" || command === "poll" || command === "scan";
}

/** The parsed CLI invocation. */
export interface CliArgs {
  /** The subcommand: `run-cycle` (default), `poll` (cron) or `scan`. */
  readonly command: string;
  /** Path to the registry manifest JSON. */
  readonly manifest: string;
  /** Watched GitHub orgs (repeatable `--org`). */
  readonly orgs: readonly string[];
  /** npm names that opted into the optional Treaty step (repeatable). */
  readonly treatyOptIn: readonly string[];
  /** Treaty authoring mode for opted-in libs, when `--treaty-opt-in` is used. */
  readonly treatyMode: TreatyAuthoringMode | undefined;
}

/** Parse argv into a {@link CliArgs}, applying defaults. */
export function parseArgs(argv: readonly string[]): CliArgs {
  const [command = "run-cycle", ...rest] = argv;
  let manifest = "registry.json";
  let treatyMode: TreatyAuthoringMode | undefined;
  const orgs: string[] = [];
  const treatyOptIn: string[] = [];
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--manifest") {
      manifest = rest[i + 1] ?? manifest;
      i += 1;
    } else if (arg === "--org") {
      const org = rest[i + 1];
      if (org !== undefined) orgs.push(org);
      i += 1;
    } else if (arg === "--treaty-opt-in") {
      const name = rest[i + 1];
      if (name !== undefined) treatyOptIn.push(name);
      i += 1;
    } else if (arg === "--treaty-mode") {
      const mode = rest[i + 1];
      if (mode === "compat" || mode === "enhanced") treatyMode = mode;
      i += 1;
    }
  }
  return { command, manifest, orgs, treatyOptIn, treatyMode };
}

/** Render a one-line-per-section human summary of a cycle result. */
export function summarize(result: MaintenanceCycleResult): string {
  const lines: string[] = [];
  lines.push(
    `cycle @ ${new Date(result.now).toISOString()}: ` +
      `${result.prsOpened} PR(s) opened, ` +
      `${result.takeoversExecuted} takeover(s) executed`,
  );
  for (const lib of result.libraries) {
    lines.push(`  - ${lib.npmName}: ${lib.result}`);
  }
  for (const takeover of result.takeovers) {
    const fired = takeover.run.decision.shouldTakeover ? "TAKEN OVER" : "held";
    lines.push(`  - takeover ${takeover.npmName}: ${fired}`);
  }
  return lines.join("\n");
}

/**
 * Build the orchestration {@link BotConfigInput} from parsed args: the watched
 * orgs and, when libraries opted into Treaty via `--treaty-opt-in`, the opt-in
 * set + mode. With no opt-ins the Treaty fields are omitted and behaviour is
 * unchanged.
 */
function botConfigFrom(args: CliArgs): BotConfigInput {
  return {
    watchedOrgs: args.orgs,
    ...(args.treatyOptIn.length > 0 ? { treatyOptIn: args.treatyOptIn } : {}),
    ...(args.treatyMode !== undefined ? { treatyMode: args.treatyMode } : {}),
  };
}

/**
 * The CLI entry. Resolves the GitHub token from the environment, loads the
 * registry manifest, builds the production runner, and runs the requested
 * command, printing a summary. The host cron invokes `poll` (and `scan`) on its
 * schedule; the deterministic orchestration logic lives in
 * `@ngx-maintenance/orchestrator`.
 *
 *  - `run-cycle` / `poll` : drive a full discover -> migrate -> PR -> takeover
 *    maintenance cycle. On a stateless CI runner `poll` always finds a cycle
 *    due (fresh scheduler state), so it is the cron entry point; the pure
 *    scheduler still gates re-runs for a long-lived host.
 *  - `scan` : a discovery cycle that does NOT carry outstanding takeover PRs,
 *    so it only opens migration PRs for newly-stale libraries.
 */
export async function main(argv: readonly string[]): Promise<number> {
  const args = parseArgs(argv);
  if (!isCliCommand(args.command)) {
    process.stderr.write(`unknown command: ${args.command}\n`);
    return 2;
  }

  const token = process.env["GITHUB_TOKEN"];
  if (token === undefined || token.length === 0) {
    process.stderr.write("GITHUB_TOKEN is required\n");
    return 1;
  }

  const latestAngular = Number(
    process.env["LATEST_ANGULAR"] ?? String(LATEST_ANGULAR),
  );
  const manifest = await loadManifest(args.manifest);
  const bot = botConfigFrom(args);
  const enableTreaty = args.treatyOptIn.length > 0;

  const runner = createRunner({
    token,
    manifest,
    latestAngular,
    bot,
    ...(enableTreaty ? { enableTreaty: true } : {}),
  });
  const now = Date.now();

  // `poll` runs through the pure scheduler so a long-lived host can gate
  // re-runs; on a stateless CI runner the fresh state is always due. `scan` and
  // `run-cycle` drive a cycle directly (scan carries no takeover PRs).
  const result: MaintenanceCycleResult =
    args.command === "poll"
      ? await runPoll(runner, now)
      : await runner.run({ now, outstandingPrs: [] });

  process.stdout.write(`${summarize(result)}\n`);
  return 0;
}

/** Drive one host poll through the pure scheduler at `now`. */
async function runPoll(
  runner: ReturnType<typeof createRunner>,
  now: number,
): Promise<MaintenanceCycleResult> {
  const outcome = await poll(
    runner,
    resolveSchedulerConfig(),
    initialState(),
    now,
  );
  if (outcome.result === undefined) {
    // Unreachable on a fresh state (always due), but keep the contract total.
    return {
      now,
      libraries: [],
      takeovers: [],
      prsOpened: 0,
      takeoversExecuted: 0,
    };
  }
  return outcome.result;
}

// Run when invoked directly (not when imported by tests).
if (
  process.argv[1] !== undefined &&
  import.meta.url === `file://${process.argv[1].replace(/\\/g, "/")}`
) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error: unknown) => {
      process.stderr.write(`${String(error)}\n`);
      process.exitCode = 1;
    });
}
