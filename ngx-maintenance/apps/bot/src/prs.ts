import type { MigrationPlan } from "@ngx-maintenance/migration-engine";
import type { TakeoverSpec } from "@ngx-maintenance/takeover";
import type { RegistryEntry, RepoMetadata } from "@ngx-maintenance/registry";
import { parseRepoRef, type OctokitLike, type RepoRef } from "./github.js";

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

/** Stable head-branch name for a migration up to a given major. */
export function migrationBranch(toMajor: number): string {
  return `ngx-maintenance/angular-${toMajor}`;
}

/** Build the deterministic migration PR spec for an opted-in library. */
export function buildMigrationPr(
  entry: Pick<RegistryEntry, "npmName" | "repoUrl">,
  plan: MigrationPlan,
  base: string,
): MigrationPrSpec {
  const head = migrationBranch(plan.to);
  const majors = plan.steps.map((step) => `v${step.from} -> v${step.to}`);
  const veToIvy = plan.steps.some((step) => step.kind === "ve-to-ivy");
  const bodyLines = [
    `Automated Angular migration for \`${entry.npmName}\`.`,
    "",
    `This chains the OFFICIAL Angular \`ng update\` schematics from v${plan.from}`,
    `to v${plan.to}, one major at a time:`,
    "",
    ...majors.map((label) => `- ${label}`),
    "",
    veToIvy
      ? "Includes the View-Engine -> Ivy transition for the v8-v12 window."
      : "All steps are post-Ivy.",
    "",
    "No AI was used: every step is a deterministic schematic + curated codemod,",
    "verified by install + build + test.",
  ];
  return {
    kind: "migration-pr",
    npmName: entry.npmName,
    repoUrl: entry.repoUrl,
    head,
    base,
    title: `Migrate ${entry.npmName} to Angular v${plan.to}`,
    body: bodyLines.join("\n"),
    plan,
  };
}

/** Build the opt-in suggestion (issue + sample PR) for a discovered lib. */
export function buildOptInSuggestion(
  metadata: RepoMetadata,
  plan: MigrationPlan,
  base: string,
): OptInSuggestionSpec {
  const samplePr = buildMigrationPr(
    { npmName: metadata.npmName, repoUrl: metadata.repoUrl },
    plan,
    base,
  );
  const issueBody = [
    `\`${metadata.npmName}\` looks like it is behind the latest Angular major`,
    `(currently on v${metadata.currentAngular}) and has not seen a commit in a`,
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
    repoUrl: metadata.repoUrl,
    npmName: metadata.npmName,
    issue: {
      title: `Keep ${metadata.npmName} on the latest Angular with ngx-maintenance`,
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

/** The work a takeover produces: the spec to spin into a new standalone repo. */
export interface TakeoverPlanned {
  readonly kind: "takeover";
  readonly spec: TakeoverSpec;
}
