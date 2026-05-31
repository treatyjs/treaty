/**
 * Minimal ambient declarations for the Node.js built-in modules this package
 * uses for manifest load/save.
 *
 * The ngx-maintenance Turborepo does not install `@types/node` per package, and
 * consumers that typecheck through our source (via tsconfig `paths`) must not be
 * forced to add `"types": ["node"]`. Declaring only the surface we use keeps the
 * registry self-contained: `tsgo --noEmit` resolves these modules without any
 * install step, and the real Node runtime supplies the implementations.
 */

declare module "node:fs/promises" {
  export function readFile(
    path: string,
    encoding: "utf8",
  ): Promise<string>;
  export function writeFile(
    path: string,
    data: string,
    encoding: "utf8",
  ): Promise<void>;
  export function mkdir(
    path: string,
    options: { recursive: boolean },
  ): Promise<string | undefined>;
  export function rm(
    path: string,
    options: { recursive: boolean; force: boolean },
  ): Promise<void>;
  export function mkdtemp(prefix: string): Promise<string>;
}

declare module "node:path" {
  export function dirname(path: string): string;
  export function join(...paths: string[]): string;
}

declare module "node:os" {
  export function tmpdir(): string;
}
