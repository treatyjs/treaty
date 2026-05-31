#!/usr/bin/env node
import process from "node:process";
import { loadManifest } from "@ngx-maintenance/registry";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";
import type {
  BotConfigInput,
  MaintenanceCycleResult,
} from "@ngx-maintenance/orchestrator";
import { createRunner } from "./runner.js";

/** The parsed CLI invocation. */
export interface CliArgs {
  /** The subcommand (`run-cycle` is the only one today). */
  readonly command: string;
  /** Path to the registry manifest JSON. */
  readonly manifest: string;
  /** Watched GitHub orgs (repeatable `--org`). */
  readonly orgs: readonly string[];
}

/** Parse argv into a {@link CliArgs}, applying defaults. */
export function parseArgs(argv: readonly string[]): CliArgs {
  const [command = "run-cycle", ...rest] = argv;
  let manifest = "registry.json";
  const orgs: string[] = [];
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--manifest") {
      manifest = rest[i + 1] ?? manifest;
      i += 1;
    } else if (arg === "--org") {
      const org = rest[i + 1];
      if (org !== undefined) orgs.push(org);
      i += 1;
    }
  }
  return { command, manifest, orgs };
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
 * The CLI entry. Resolves the GitHub token from the environment, loads the
 * registry manifest, builds the production runner, and runs ONE maintenance
 * cycle, printing a summary. The host cron invokes this on its schedule; the
 * deterministic orchestration logic lives in `@ngx-maintenance/orchestrator`.
 */
export async function main(argv: readonly string[]): Promise<number> {
  const args = parseArgs(argv);
  if (args.command !== "run-cycle") {
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
  const bot: BotConfigInput = { watchedOrgs: args.orgs };

  const runner = createRunner({ token, manifest, latestAngular, bot });
  const result = await runner.run({ now: Date.now() });

  process.stdout.write(`${summarize(result)}\n`);
  return 0;
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
