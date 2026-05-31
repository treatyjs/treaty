import type { GitHubAdapter } from "./adapter.js";
import type { OctokitLike } from "./octokit.js";
import { createFakeShell, type FakeShell } from "./shell.js";
import { createFakeWorkdirs, type FakeWorkdirProvider } from "./workdir.js";

/**
 * Programs how the fake octokit's `pulls.list` responds: maps the list params
 * to the PR array to return. When omitted (or it returns `undefined`) the fake
 * returns an empty list — i.e. no existing open PR, so the orchestrator opens
 * one. Tests pass a responder to simulate an already-open migration PR.
 */
export type PullListResponder = (
  params: Record<string, unknown>,
) => ReadonlyArray<Record<string, unknown>> | undefined;

/** A recording fake octokit that satisfies {@link OctokitLike}. */
export interface FakeOctokit extends OctokitLike {
  /** Every `pulls.create` params object, in call order. */
  readonly pullCalls: ReadonlyArray<Record<string, unknown>>;
  /** Every `pulls.list` params object, in call order. */
  readonly pullListCalls: ReadonlyArray<Record<string, unknown>>;
  /** Every `issues.create` params object, in call order. */
  readonly issueCalls: ReadonlyArray<Record<string, unknown>>;
}

/**
 * Build a recording fake octokit. Each create call is captured and resolves
 * with a deterministic incrementing `number`, so tests can assert exactly which
 * PRs/issues were opened and with what params — without any network call. The
 * optional `listResponder` programs `pulls.list` so a test can simulate an
 * already-open migration PR (driving the orchestrator's idempotency path).
 * This is the canonical fake the whole monorepo shares for the GitHub boundary.
 */
export function createFakeOctokit(listResponder?: PullListResponder): FakeOctokit {
  const pullCalls: Array<Record<string, unknown>> = [];
  const pullListCalls: Array<Record<string, unknown>> = [];
  const issueCalls: Array<Record<string, unknown>> = [];
  return {
    pullCalls,
    pullListCalls,
    issueCalls,
    rest: {
      pulls: {
        create(params) {
          pullCalls.push(params);
          return Promise.resolve({ data: { number: pullCalls.length } });
        },
        list(params) {
          pullListCalls.push(params);
          return Promise.resolve({ data: listResponder?.(params) ?? [] });
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

/** A fake adapter exposing the recording octokit + shell + workdirs. */
export interface FakeAdapter extends GitHubAdapter {
  readonly octokit: FakeOctokit;
  readonly fakeShell: FakeShell;
  readonly workdirs: FakeWorkdirProvider;
}

/** Options for {@link createFakeAdapter}. */
export interface FakeAdapterOptions {
  /** Programs the recording fake shell's per-argv results. */
  readonly shellResponder?: Parameters<typeof createFakeShell>[0];
  /** Programs the fake octokit's `pulls.list` (simulate an existing open PR). */
  readonly listResponder?: PullListResponder;
}

/**
 * Build a fully fake {@link GitHubAdapter} for unit tests: a recording octokit
 * plus a recording shell plus deterministic in-memory workdirs. No real
 * network, process or filesystem call occurs. Pass a `shellResponder` to
 * program command results (e.g. inject a failing migration step) and a
 * `listResponder` to simulate an already-open PR.
 *
 * For backwards compatibility a bare shell responder may be passed positionally.
 */
export function createFakeAdapter(
  options?: FakeAdapterOptions | Parameters<typeof createFakeShell>[0],
): FakeAdapter {
  const opts: FakeAdapterOptions =
    typeof options === "function" ? { shellResponder: options } : options ?? {};
  const octokit = createFakeOctokit(opts.listResponder);
  const fakeShell = createFakeShell(opts.shellResponder);
  const workdirs = createFakeWorkdirs();
  return { octokit, shell: fakeShell.shell, fakeShell, workdirs };
}
