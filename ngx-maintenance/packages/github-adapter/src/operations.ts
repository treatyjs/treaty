import type { MigrationPlan } from "@ngx-maintenance/migration-engine";
import type { TakeoverSpec } from "@ngx-maintenance/takeover";
import { parseRepoRef, type OctokitLike, type RepoRef } from "./octokit.js";
import type { Shell } from "./shell.js";

/**
 * The minimal library description {@link buildMigrationPr} needs. A structural
 * subset of the registry's entry shape, so a `RegistryEntry` (or any object
 * carrying these fields) can be passed directly without the adapter depending
 * on the registry package — keeping this the disjoint GitHub/git/process
 * boundary.
 */
export interface PrLib {
  /** npm package name of the target library. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
}

/**
 * The minimal metadata {@link buildOptInSuggestion} needs about a discovered
 * library. A structural subset of the registry's `RepoMetadata`.
 */
export interface DiscoveredLib {
  readonly npmName: string;
  readonly repoUrl: string;
  /** The Angular major this library currently targets. */
  readonly currentAngular: number;
}

/** A migration pull request the bot intends to open for an opted-in library. */
export interface MigrationPrSpec {
  readonly kind: "migration-pr";
  /** npm package name of the target library. */
  readonly npmName: string;
  /** The repository the PR targets. */
  readonly repoUrl: string;
  /** The branch the bot pushes the migration onto. */
  readonly head: string;
  /** The base branch the PR merges into. */
  readonly base: string;
  readonly title: string;
  readonly body: string;
  /** The migration chain that motivated the PR (from..to with steps). */
  readonly plan: MigrationPlan;
}

/**
 * An opt-in suggestion the bot opens against a discovered, stale, unregistered
 * library: an issue suggesting the App plus a sample migration PR.
 */
export interface OptInSuggestionSpec {
  readonly kind: "opt-in-suggestion";
  readonly repoUrl: string;
  readonly npmName: string;
  /** The suggestion issue. */
  readonly issue: { readonly title: string; readonly body: string };
  /** A sample migration PR demonstrating the bot's value. */
  readonly samplePr: MigrationPrSpec;
}

/** The work a takeover produces: the spec to spin into a new standalone repo. */
export interface TakeoverPlanned {
  readonly kind: "takeover";
  readonly spec: TakeoverSpec;
}

/** Stable head-branch name for a migration up to a given major. */
export function migrationBranch(toMajor: number): string {
  return `ngx-maintenance/angular-${toMajor}`;
}

/** Build the deterministic migration PR spec for an opted-in library. */
export function buildMigrationPr(
  lib: PrLib,
  plan: MigrationPlan,
  base: string,
): MigrationPrSpec {
  const head = migrationBranch(plan.to);
  const majors = plan.steps.map((step) => `v${step.from} -> v${step.to}`);
  const veToIvy = plan.steps.some((step) => step.kind === "ve-to-ivy");
  const bodyLines = [
    `Automated Angular migration for \`${lib.npmName}\`.`,
    "",
    `This chains the OFFICIAL Angular \`ng update\` schematics from v${plan.from}`,
    `to v${plan.to}, one major at a time:`,
    "",
    ...majors.map((label) => `- ${label}`),
    "",
    veToIvy
      ? "Includes the View-Engine -> Ivy transition for the v9-v12 window."
      : "All steps are post-Ivy.",
    "",
    "No AI was used: every step is a deterministic schematic + curated codemod,",
    "verified by install + build + test.",
  ];
  return {
    kind: "migration-pr",
    npmName: lib.npmName,
    repoUrl: lib.repoUrl,
    head,
    base,
    title: `Migrate ${lib.npmName} to Angular v${plan.to}`,
    body: bodyLines.join("\n"),
    plan,
  };
}

/** Build the opt-in suggestion (issue + sample PR) for a discovered lib. */
export function buildOptInSuggestion(
  lib: DiscoveredLib,
  plan: MigrationPlan,
  base: string,
): OptInSuggestionSpec {
  const samplePr = buildMigrationPr(
    { npmName: lib.npmName, repoUrl: lib.repoUrl },
    plan,
    base,
  );
  const issueBody = [
    `\`${lib.npmName}\` looks like it is behind the latest Angular major`,
    `(currently on v${lib.currentAngular}) and has not seen a commit in a`,
    "while.",
    "",
    "ngx-maintenance can keep it current automatically. Install the GitHub App",
    "and every future Angular release opens a deterministic migration PR (no AI).",
    "",
    `We have opened a sample migration PR (\`${samplePr.head}\`) so you can see`,
    "exactly what the automated upgrade would look like.",
  ].join("\n");
  return {
    kind: "opt-in-suggestion",
    repoUrl: lib.repoUrl,
    npmName: lib.npmName,
    issue: {
      title: `Keep ${lib.npmName} on the latest Angular with ngx-maintenance`,
      body: issueBody,
    },
    samplePr,
  };
}

/** Open a migration PR via octokit. Returns the parsed target ref. */
export async function openMigrationPr(
  octokit: OctokitLike,
  spec: MigrationPrSpec,
): Promise<RepoRef> {
  const ref = parseRepoRef(spec.repoUrl);
  if (ref === undefined) {
    throw new Error(`cannot parse repo from URL: ${spec.repoUrl}`);
  }
  await octokit.rest.pulls.create({
    owner: ref.owner,
    repo: ref.repo,
    title: spec.title,
    head: spec.head,
    base: spec.base,
    body: spec.body,
  });
  return ref;
}

/** Open an opt-in suggestion issue (and its sample PR) via octokit. */
export async function openOptInSuggestion(
  octokit: OctokitLike,
  spec: OptInSuggestionSpec,
): Promise<RepoRef> {
  const ref = parseRepoRef(spec.repoUrl);
  if (ref === undefined) {
    throw new Error(`cannot parse repo from URL: ${spec.repoUrl}`);
  }
  await octokit.rest.issues.create({
    owner: ref.owner,
    repo: ref.repo,
    title: spec.issue.title,
    body: spec.issue.body,
  });
  await openMigrationPr(octokit, spec.samplePr);
  return ref;
}

/** The result of cloning a repository into a working directory. */
export interface CloneResult {
  /** The directory the repository was checked out into. */
  readonly dir: string;
  /** Whether the clone (and optional checkout) succeeded. */
  readonly ok: boolean;
  /** Combined output of the git commands run, for logging on failure. */
  readonly output: string;
}

/** Options controlling a {@link clone}. */
export interface CloneOptions {
  /** A specific branch/ref to check out after cloning. */
  readonly ref?: string;
  /** Shallow-clone depth; omitted means a full clone. */
  readonly depth?: number;
}

/**
 * Clone `repoUrl` into `dir` by shelling `git` through the injected
 * {@link Shell}. This is the git boundary: production passes a real spawning
 * shell; tests pass a recording fake so no network/process call occurs. The
 * clone runs in `dir`'s parent so `git` creates `dir` itself; an optional ref
 * is checked out afterward. Stops at the first non-zero git exit.
 */
export async function clone(
  shell: Shell,
  repoUrl: string,
  dir: string,
  options: CloneOptions = {},
): Promise<CloneResult> {
  const argv: string[] = ["git", "clone"];
  if (options.depth !== undefined) argv.push("--depth", String(options.depth));
  argv.push(repoUrl, dir);

  const cloneResult = await shell(parentDir(dir), argv);
  let output = cloneResult.output;
  if (cloneResult.code !== 0) {
    return { dir, ok: false, output };
  }

  if (options.ref !== undefined) {
    const checkout = await shell(dir, ["git", "checkout", options.ref]);
    output = `${output}\n${checkout.output}`;
    if (checkout.code !== 0) {
      return { dir, ok: false, output };
    }
  }

  return { dir, ok: true, output };
}

/**
 * The parent directory of a path, computed without `node:path` so the function
 * stays pure and dependency-free. Handles both `/` and `\\` separators; returns
 * `.` when the path has no separator (clone-in-place).
 */
export function parentDir(dir: string): string {
  const normalized = dir.replace(/[/\\]+$/, "");
  const lastSep = Math.max(
    normalized.lastIndexOf("/"),
    normalized.lastIndexOf("\\"),
  );
  if (lastSep <= 0) return lastSep === 0 ? normalized.slice(0, 1) : ".";
  return normalized.slice(0, lastSep);
}
