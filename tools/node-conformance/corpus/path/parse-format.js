// CONFORMANCE: skip — node:path parse()/format()/toNamespacedPath()/matchesGlob() are not implemented
// in the Treaty runtime yet (path.parse, path.format, path.posix.parse/format, path.win32.parse and
// path.toNamespacedPath all read back as `undefined`). When they land, this file should assert the
// round-trip identity `path.format(path.parse(p))` re-derives `p` and that parse yields the
// {root, dir, base, ext, name} record, on the platform default plus the posix and win32 namespaces.
//
// This file is intentionally not evaluated; it documents the precise gap so the conformance
// pass-rate is not inflated by pretending the parse/format surface exists.
const path = require("node:path");

test("path.parse decomposes into {root, dir, base, ext, name}", () => {
  const parsed = path.posix.parse("/home/user/file.txt");
  assert.strictEqual(parsed.root, "/", "root");
  assert.strictEqual(parsed.dir, "/home/user", "dir");
  assert.strictEqual(parsed.base, "file.txt", "base");
  assert.strictEqual(parsed.ext, ".txt", "ext");
  assert.strictEqual(parsed.name, "file", "name");
});

test("path.format is the inverse of path.parse", () => {
  const p = "/home/user/file.txt";
  assert.strictEqual(path.posix.format(path.posix.parse(p)), p, "format(parse(p)) === p");
});
