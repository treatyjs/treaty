/**
 * Test-only stub for the out-of-band `octokit` package. The real Octokit is
 * supplied out-of-band at deploy time and is not installed in this Turborepo,
 * so the vitest config aliases `octokit` to this stub. Tests inject recording
 * fakes via {@link createFakeAdapter}; the production {@link createGitHubAdapter}
 * path constructs this stub only to prove it is wired, never to make a call.
 */
export class Octokit {
  readonly rest = {
    pulls: {
      create: () => Promise.resolve({ data: {} }),
    },
    issues: {
      create: () => Promise.resolve({ data: {} }),
    },
  };

  /** Captured construction options; the stub never authenticates or calls out. */
  readonly options: { auth?: string; [key: string]: unknown };

  constructor(options?: { auth?: string; [key: string]: unknown }) {
    this.options = options ?? {};
  }
}
