import type {
  InstallationEvent,
  PushEvent,
  ReleaseEvent,
} from "@octokit/webhooks-types";
import { markAppInstalled } from "@ngx-maintenance/registry";
import { planChain } from "@ngx-maintenance/migration-engine";
import type { BotContext } from "./context.js";
import { repoUrlMatchesFullName } from "./github.js";
import {
  buildMigrationPr,
  openMigrationPr,
  type MigrationPrSpec,
} from "./prs.js";

/** The outcome of handling a webhook event. */
export interface HandlerResult {
  /** A short machine-readable action label. */
  readonly action: string;
  /** Repositories affected by the handler. */
  readonly repos: readonly string[];
  /** The next manifest state (handlers are pure w.r.t. the manifest). */
  readonly manifest: BotContext["manifest"];
}

/** The result of an Angular-release roll: the planned + opened PR set. */
export interface ReleaseResult extends HandlerResult {
  /** The migration PRs computed for every behind, app-installed library. */
  readonly prs: readonly MigrationPrSpec[];
}

/** Parse an Angular release tag (`vNN.x.y` or `NN.x.y`) into its major. */
export function releaseMajor(tag: string): number {
  const match = /v?(\d+)\./.exec(tag);
  return match ? Number(match[1]) : Number.NaN;
}

/**
 * Installation handler: when a repo installs the App, mark exactly the
 * libraries whose repository matches an installed repo as app-installed in the
 * manifest, enabling auto-roll on future releases. Repos not in the registry
 * are reported but do not mutate the manifest.
 */
export function onInstallation(
  ctx: BotContext,
  event: InstallationEvent,
): HandlerResult {
  const installedFullNames = (event.repositories ?? []).map(
    (repo) => repo.full_name,
  );
  let manifest = ctx.manifest;
  for (const entry of ctx.manifest.entries) {
    const matched = installedFullNames.some((fullName) =>
      repoUrlMatchesFullName(entry.repoUrl, fullName),
    );
    if (matched) manifest = markAppInstalled(manifest, entry.npmName);
  }
  return { action: "app-installed", repos: installedFullNames, manifest };
}

/**
 * Angular release handler: on a new `@angular/core` major, compute and open a
 * migration PR for every opted-in, app-installed library that is behind the
 * release. The PR set is computed deterministically from the manifest; opening
 * is delegated to octokit so the computation stays unit-testable.
 */
export async function onAngularRelease(
  ctx: BotContext,
  event: ReleaseEvent,
): Promise<ReleaseResult> {
  const latest = releaseMajor(event.release.tag_name);
  const prs: MigrationPrSpec[] = [];
  if (Number.isFinite(latest)) {
    for (const entry of ctx.manifest.entries) {
      if (!entry.appInstalled) continue;
      const plan = planChain(entry.currentAngular, latest);
      if (plan.steps.length === 0) continue;
      prs.push(buildMigrationPr(entry, plan, ctx.config.baseBranch));
    }
  }
  for (const spec of prs) {
    // Sequential: PRs are independent but we keep ordering deterministic and
    // avoid hammering the API with an unbounded burst.
    // eslint-disable-next-line no-await-in-loop
    await openMigrationPr(ctx.octokit, spec);
  }
  return {
    action: "migration-prs-opened",
    repos: prs.map((spec) => spec.repoUrl),
    manifest: ctx.manifest,
    prs,
  };
}

/** Push handler: keep registry metadata fresh for the pushed repository. */
export function onPush(ctx: BotContext, event: PushEvent): HandlerResult {
  return {
    action: "push-observed",
    repos: [event.repository.full_name],
    manifest: ctx.manifest,
  };
}
