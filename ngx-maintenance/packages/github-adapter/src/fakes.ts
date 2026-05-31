import type { GitHubAdapter } from "./adapter.js";
import type { OctokitLike } from "./octokit.js";
import { createFakeShell, type FakeShell } from "./shell.js";

/** A recording fake octokit that satisfies {@link OctokitLike}. */
export interface FakeOctokit extends OctokitLike {
  /** Every `pulls.create` params object, in call order. */
  readonly pullCalls: ReadonlyArray<Record<string, unknown>>;
  /** Every `issues.create` params object, in call order. */
  readonly issueCalls: ReadonlyArray<Record<string, unknown>>;
}

/**
 * Build a recording fake octokit. Each create call is captured and resolves
 * with a deterministic incrementing `number`, so tests can assert exactly which
 * PRs/issues were opened and with what params — without any network call. This
 * is the canonical fake the whole monorepo shares for the GitHub boundary.
 */
export function createFakeOctokit(): FakeOctokit {
  const pullCalls: Array<Record<string, unknown>> = [];
  const issueCalls: Array<Record<string, unknown>> = [];
  return {
    pullCalls,
    issueCalls,
    rest: {
      pulls: {
        create(params) {
          pullCalls.push(params);
          return Promise.resolve({ data: { number: pullCalls.length } });
        },
      },
      issues: {
        create(params) {
          issueCalls.push(params);
          return Promise.resolve({ data: { number: issueCalls.length } });
        },
      },
    },
  };
}

/** A fake adapter exposing the recording octokit + shell for assertions. */
export interface FakeAdapter extends GitHubAdapter {
  readonly octokit: FakeOctokit;
  readonly fakeShell: FakeShell;
}

/**
 * Build a fully fake {@link GitHubAdapter} for unit tests: a recording octokit
 * plus a recording shell. No real network or process call occurs. Pass through
 * the shell `responder` to program command results (e.g. inject a failing step).
 */
export function createFakeAdapter(
  shellResponder?: Parameters<typeof createFakeShell>[0],
): FakeAdapter {
  const octokit = createFakeOctokit();
  const fakeShell = createFakeShell(shellResponder);
  return { octokit, shell: fakeShell.shell, fakeShell };
}
