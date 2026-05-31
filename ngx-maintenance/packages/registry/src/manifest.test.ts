import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtemp, rm, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createManifest,
  findEntry,
  upsertEntry,
  removeEntry,
  markAppInstalled,
  parseManifest,
  serializeManifest,
  loadManifest,
  saveManifest,
} from "./manifest.js";
import {
  MANIFEST_VERSION,
  type RegistryEntry,
  type RegistryManifest,
} from "./types.js";

function entry(overrides: Partial<RegistryEntry> = {}): RegistryEntry {
  return {
    npmName: "@acme/widget",
    repoUrl: "https://github.com/acme/widget",
    currentAngular: 17,
    appInstalled: false,
    ...overrides,
  };
}

describe("createManifest", () => {
  it("creates an empty manifest at the current schema version", () => {
    const manifest = createManifest();
    expect(manifest.version).toBe(MANIFEST_VERSION);
    expect(manifest.entries).toEqual([]);
  });
});

describe("findEntry / upsertEntry / removeEntry", () => {
  it("upsert appends a new entry without mutating the input", () => {
    const before = createManifest([entry({ npmName: "@a/one" })]);
    const after = upsertEntry(before, entry({ npmName: "@b/two" }));
    expect(before.entries).toHaveLength(1);
    expect(after.entries).toHaveLength(2);
    expect(findEntry(after, "@b/two")).toBeDefined();
  });

  it("upsert replaces in place, preserving position", () => {
    const manifest = createManifest([
      entry({ npmName: "@a/one", currentAngular: 15 }),
      entry({ npmName: "@b/two", currentAngular: 16 }),
    ]);
    const updated = upsertEntry(
      manifest,
      entry({ npmName: "@a/one", currentAngular: 22 }),
    );
    expect(updated.entries.map((e) => e.npmName)).toEqual(["@a/one", "@b/two"]);
    expect(findEntry(updated, "@a/one")?.currentAngular).toBe(22);
  });

  it("removeEntry drops the matching entry", () => {
    const manifest = createManifest([
      entry({ npmName: "@a/one" }),
      entry({ npmName: "@b/two" }),
    ]);
    const removed = removeEntry(manifest, "@a/one");
    expect(removed.entries.map((e) => e.npmName)).toEqual(["@b/two"]);
  });
});

describe("markAppInstalled", () => {
  it("flips appInstalled for an existing entry", () => {
    const manifest = createManifest([entry({ appInstalled: false })]);
    const updated = markAppInstalled(manifest, "@acme/widget");
    expect(findEntry(updated, "@acme/widget")?.appInstalled).toBe(true);
  });

  it("is a no-op for an unknown entry", () => {
    const manifest = createManifest([entry()]);
    expect(markAppInstalled(manifest, "@nope/missing")).toBe(manifest);
  });
});

describe("parseManifest", () => {
  it("accepts a well-formed manifest", () => {
    const raw = {
      version: 1,
      entries: [
        {
          npmName: "@a/one",
          repoUrl: "https://example.com",
          currentAngular: 17,
          appInstalled: true,
        },
      ],
    };
    const parsed = parseManifest(raw);
    expect(parsed.entries).toHaveLength(1);
  });

  it("rejects a non-object", () => {
    expect(() => parseManifest(42)).toThrow(/not an object/);
  });

  it("rejects a missing version", () => {
    expect(() => parseManifest({ entries: [] })).toThrow(/invalid version/);
  });

  it("rejects non-array entries", () => {
    expect(() => parseManifest({ version: 1, entries: {} })).toThrow(
      /not an array/,
    );
  });

  it("rejects an entry with a bad field", () => {
    const raw = {
      version: 1,
      entries: [{ npmName: "", repoUrl: "x", currentAngular: 1, appInstalled: true }],
    };
    expect(() => parseManifest(raw)).toThrow(/invalid npmName/);
  });

  it("rejects a non-integer Angular major", () => {
    const raw = {
      version: 1,
      entries: [
        { npmName: "@a/x", repoUrl: "x", currentAngular: 1.5, appInstalled: true },
      ],
    };
    expect(() => parseManifest(raw)).toThrow(/invalid currentAngular/);
  });

  it("rejects duplicate npm names", () => {
    const raw = {
      version: 1,
      entries: [
        { npmName: "@a/x", repoUrl: "x", currentAngular: 1, appInstalled: true },
        { npmName: "@a/x", repoUrl: "y", currentAngular: 2, appInstalled: false },
      ],
    };
    expect(() => parseManifest(raw)).toThrow(/duplicate/);
  });
});

describe("serializeManifest", () => {
  it("round-trips through parseManifest", () => {
    const manifest = createManifest([entry()]);
    const text = serializeManifest(manifest);
    expect(text.endsWith("\n")).toBe(true);
    const reparsed = parseManifest(JSON.parse(text));
    expect(reparsed).toEqual(manifest);
  });
});

describe("loadManifest / saveManifest (filesystem)", () => {
  let dir: string;

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "ngx-registry-"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  it("returns a fresh empty manifest when the file is absent", async () => {
    const manifest = await loadManifest(join(dir, "missing.json"));
    expect(manifest.version).toBe(MANIFEST_VERSION);
    expect(manifest.entries).toEqual([]);
  });

  it("saves and reloads a manifest, creating nested directories", async () => {
    const path = join(dir, "nested", "registry.json");
    const manifest: RegistryManifest = createManifest([
      entry({ npmName: "@a/one", appInstalled: true }),
      entry({ npmName: "@b/two", currentAngular: 20 }),
    ]);
    await saveManifest(path, manifest);
    const reloaded = await loadManifest(path);
    expect(reloaded).toEqual(manifest);
  });

  it("propagates an error for malformed JSON on disk", async () => {
    const path = join(dir, "bad.json");
    await writeFile(path, "{ not json", "utf8");
    await expect(loadManifest(path)).rejects.toThrow();
  });

  it("writes deterministic pretty-printed JSON", async () => {
    const path = join(dir, "registry.json");
    await saveManifest(path, createManifest([entry()]));
    const text = await readFile(path, "utf8");
    expect(text).toContain('"version": 1');
    expect(text).toContain('"npmName": "@acme/widget"');
  });
});
