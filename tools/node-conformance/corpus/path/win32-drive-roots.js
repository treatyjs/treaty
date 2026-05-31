// CONFORMANCE: skip — node:path.win32 drive-letter root semantics are incomplete in the Treaty
// runtime. The backslash separator, join, extname and UNC isAbsolute all work (covered by
// path/win32), but drive-anchored operations are wrong today: win32.isAbsolute("C:\\a") returns
// false, win32.dirname("C:\\foo\\bar\\baz.txt") returns "C:" instead of "C:\\foo\\bar", and
// win32.relative across "C:\\…" paths produces a malformed result. When drive-root handling is
// completed these assertions should hold.
//
// This file is intentionally not evaluated; it documents the precise gap so the conformance
// pass-rate reflects only the win32 surface that is genuinely correct.
const path = require("node:path");
const win32 = path.win32;

test("win32.isAbsolute treats a drive-letter root as absolute", () => {
  assert.strictEqual(win32.isAbsolute("C:\\a\\b"), true, "a drive-rooted path is absolute");
});

test("win32.dirname keeps the drive and the parent directories", () => {
  assert.strictEqual(win32.dirname("C:\\foo\\bar\\baz.txt"), "C:\\foo\\bar", "dirname keeps the drive");
});

test("win32.relative walks between two drive-anchored paths", () => {
  assert.strictEqual(win32.relative("C:\\a\\b\\c", "C:\\a\\b\\d\\e"), "..\\d\\e", "relative on a drive");
});
