/**
 * Test-only stub for the out-of-band `octokit` package. The real Octokit is
 * supplied out-of-band at deploy time and is not installed in this Turborepo,
 * so the vitest config aliases `octokit` to this stub. Bot tests inject fakes
 * (a fake GitHub adapter + fake metadata + fake takeover adapter) via
 * `createRunnerFrom`; the production `createRunner` path constructs this stub
 * only to prove wiring, never to make a network call.
 */
export class Octokit {
  readonly rest = {
    pulls: {
      create: () => Promise.resolve({ data: {} }),
      list: () => Promise.resolve({ data: [] as unknown[] }),
    },
    issues: {
      create: () => Promise.resolve({ data: {} }),
    },
    repos: {
      createFork: () => Promise.resolve({ data: {} }),
      listCommits: () => Promise.resolve({ data: [] as unknown[] }),
    },
  };

  readonly options: { auth?: string; [key: string]: unknown };

  constructor(options?: { auth?: string; [key: string]: unknown }) {
    this.options = options ?? {};
  }

  request(): Promise<{ status: number; data: unknown }> {
    return Promise.resolve({ status: 201, data: {} });
  }
}
