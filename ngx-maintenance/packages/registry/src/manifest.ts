import { readFile, writeFile, mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import {
  MANIFEST_VERSION,
  type RegistryEntry,
  type RegistryManifest,
} from "./types.js";

/** Create an empty manifest at the current schema version. */
export function createManifest(
  entries: readonly RegistryEntry[] = [],
): RegistryManifest {
  return { version: MANIFEST_VERSION, entries };
}

/** Find an entry by npm package name. */
export function findEntry(
  manifest: RegistryManifest,
  npmName: string,
): RegistryEntry | undefined {
  return manifest.entries.find((entry) => entry.npmName === npmName);
}

/**
 * Insert or replace an entry (keyed by npm name), returning a new manifest.
 * The manifest is treated as immutable. A replacement keeps the entry's original
 * position; a fresh entry is appended.
 */
export function upsertEntry(
  manifest: RegistryManifest,
  entry: RegistryEntry,
): RegistryManifest {
  const index = manifest.entries.findIndex(
    (existing) => existing.npmName === entry.npmName,
  );
  if (index === -1) {
    return { version: manifest.version, entries: [...manifest.entries, entry] };
  }
  const entries = manifest.entries.slice();
  entries[index] = entry;
  return { version: manifest.version, entries };
}

/** Remove an entry by npm name, returning a new manifest. */
export function removeEntry(
  manifest: RegistryManifest,
  npmName: string,
): RegistryManifest {
  return {
    version: manifest.version,
    entries: manifest.entries.filter((entry) => entry.npmName !== npmName),
  };
}

/** Mark a library's GitHub App as installed, enabling auto-roll on release. */
export function markAppInstalled(
  manifest: RegistryManifest,
  npmName: string,
): RegistryManifest {
  const entry = findEntry(manifest, npmName);
  if (entry === undefined) return manifest;
  return upsertEntry(manifest, { ...entry, appInstalled: true });
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseEntry(value: unknown, index: number): RegistryEntry {
  if (!isObject(value)) {
    throw new TypeError(`registry entry [${index}] is not an object`);
  }
  const { npmName, repoUrl, currentAngular, appInstalled } = value;
  if (typeof npmName !== "string" || npmName.length === 0) {
    throw new TypeError(`registry entry [${index}] has an invalid npmName`);
  }
  if (typeof repoUrl !== "string" || repoUrl.length === 0) {
    throw new TypeError(`registry entry [${index}] has an invalid repoUrl`);
  }
  if (typeof currentAngular !== "number" || !Number.isInteger(currentAngular)) {
    throw new TypeError(
      `registry entry [${index}] has an invalid currentAngular`,
    );
  }
  if (typeof appInstalled !== "boolean") {
    throw new TypeError(
      `registry entry [${index}] has an invalid appInstalled`,
    );
  }
  return { npmName, repoUrl, currentAngular, appInstalled };
}

/**
 * Parse and validate a manifest from a raw object (e.g. `JSON.parse` output).
 * Throws a {@link TypeError} on any malformed field so callers never operate on
 * a partially-valid registry.
 */
export function parseManifest(raw: unknown): RegistryManifest {
  if (!isObject(raw)) {
    throw new TypeError("manifest is not an object");
  }
  const { version, entries } = raw;
  if (typeof version !== "number" || !Number.isInteger(version)) {
    throw new TypeError("manifest has an invalid version");
  }
  if (!Array.isArray(entries)) {
    throw new TypeError("manifest entries is not an array");
  }
  const parsed = entries.map((entry, index) => parseEntry(entry, index));
  const seen = new Set<string>();
  for (const entry of parsed) {
    if (seen.has(entry.npmName)) {
      throw new TypeError(`duplicate registry entry for ${entry.npmName}`);
    }
    seen.add(entry.npmName);
  }
  return { version, entries: parsed };
}

/** Serialize a manifest to a stable, pretty-printed JSON string. */
export function serializeManifest(manifest: RegistryManifest): string {
  const validated = parseManifest(manifest);
  return `${JSON.stringify(validated, undefined, 2)}\n`;
}

/**
 * Load and validate a manifest from disk. A missing file yields a fresh empty
 * manifest so first-run is seamless; malformed JSON or schema is surfaced as an
 * error.
 */
export async function loadManifest(path: string): Promise<RegistryManifest> {
  let text: string;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if (isObject(error) && error["code"] === "ENOENT") {
      return createManifest();
    }
    throw error;
  }
  return parseManifest(JSON.parse(text));
}

/** Validate and write a manifest to disk, creating parent directories. */
export async function saveManifest(
  path: string,
  manifest: RegistryManifest,
): Promise<void> {
  const text = serializeManifest(manifest);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, text, "utf8");
}
