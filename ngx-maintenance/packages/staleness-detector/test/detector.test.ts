import { describe, it, expect } from "vitest";
import { StalenessDetector } from "../src/detector.js";
import { DAY_MS, type MetadataSource, type RepoMetadata } from "../src/types.js";

const NOW = Date.UTC(2026, 4, 29);
const SEVEN_MONTHS = DAY_MS * 30 * 7;

/** A deterministic in-memory MetadataSource fake — no network. */
class FakeSource implements MetadataSource {
  constructor(
    private readonly latest: number,
    private readonly repos: readonly RepoMetadata[],
  ) {}

  fetchMetadata(npmName: string): Promise<RepoMetadata | undefined> {
    return Promise.resolve(this.repos.find((r) => r.npmName === npmName));
  }

  fetchLatestAngularMajor(): Promise<number> {
    return Promise.resolve(this.latest);
  }

  listCandidates(): Promise<readonly RepoMetadata[]> {
    return Promise.resolve(this.repos);
  }
}

function repo(overrides: Partial<RepoMetadata> = {}): RepoMetadata {
  return {
    npmName: "@acme/widget",
    repoUrl: "https://github.com/acme/widget",
    currentAngular: 17,
    lastCommitMs: NOW - DAY_MS,
    ...overrides,
  };
}

describe("StalenessDetector.evaluate (via injected fake)", () => {
  it("flags a stale lib using the source's latest major", async () => {
    const source = new FakeSource(22, [
      repo({ currentAngular: 15, lastCommitMs: NOW - SEVEN_MONTHS }),
    ]);
    const candidate = await new StalenessDetector(source).evaluate(
      "@acme/widget",
      NOW,
    );
    expect(candidate?.stale).toBe(true);
    expect(candidate?.suggestOptIn).toBe(true);
  });

  it("does not flag an up-to-date lib", async () => {
    const source = new FakeSource(22, [
      repo({ currentAngular: 22, lastCommitMs: NOW - DAY_MS }),
    ]);
    const candidate = await new StalenessDetector(source).evaluate(
      "@acme/widget",
      NOW,
    );
    expect(candidate?.stale).toBe(false);
  });

  it("returns undefined for an unresolvable package", async () => {
    const source = new FakeSource(22, []);
    expect(
      await new StalenessDetector(source).evaluate("@missing/pkg", NOW),
    ).toBeUndefined();
  });

  it("honours an explicit latestAngular override without querying the source", async () => {
    const source = new FakeSource(99, [
      repo({ currentAngular: 21, lastCommitMs: NOW - SEVEN_MONTHS }),
    ]);
    const candidate = await new StalenessDetector(source).evaluate(
      "@acme/widget",
      NOW,
      22,
    );
    expect(candidate?.behindLatest).toBe(true);
  });
});

describe("StalenessDetector.discover (via injected fake)", () => {
  it("returns only the stale candidates in source order", async () => {
    const source = new FakeSource(22, [
      repo({ npmName: "@a/stale", currentAngular: 14, lastCommitMs: NOW - SEVEN_MONTHS }),
      repo({ npmName: "@b/fresh", currentAngular: 22, lastCommitMs: NOW - DAY_MS }),
      repo({ npmName: "@c/stale", currentAngular: 18, lastCommitMs: NOW - SEVEN_MONTHS }),
    ]);
    const findings = await new StalenessDetector(source).discover(NOW);
    expect(findings.map((f) => f.metadata.npmName)).toEqual(["@a/stale", "@c/stale"]);
  });
});
