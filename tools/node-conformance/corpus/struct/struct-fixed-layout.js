// CONFORMANCE: skip — struct: unimplemented (TC39 JavaScript Structs, Stage 2; Node --harmony-struct)
//
// TC39 "JavaScript Structs: Fixed Layout Objects" (https://github.com/tc39/proposal-structs).
// An unshared `struct` is a class-shaped declaration created at integrity level *sealed* → fixed
// layout: only the declared fields exist, none can be added/`delete`d, the prototype cannot be
// reassigned, and every declared field is pre-initialized to `undefined` before any user code can
// observe the instance. Methods are non-generic (their `this` must be an instance of the struct or a
// subclass). This case is intentionally skipped — the Treaty runtime ships no struct support yet and
// the compiler does not lower `struct` to a sealed class (M4). Delete this directive (and the matching
// known-unsupported.json entry) once unshared-struct lowering lands. The body below is the test we
// WILL run at that point; it must never execute while skipped, so it touches the unimplemented surface
// directly (Node only parses `struct` under --harmony-struct).

struct Box {
  x;
  y;
  constructor(x, y) {
    this.x = x;
    this.y = y;
  }
  sum() {
    return this.x + this.y;
  }
}

const b = new Box(2, 3);
if (b.sum() !== 5) throw new Error("struct method must observe pre-initialized fields");

// Fixed layout: a struct is sealed, so adding an undeclared field must throw in strict mode.
let threw = false;
try {
  "use strict";
  b.z = 9;
} catch (e) {
  threw = true;
}
if (!threw) throw new Error("a struct instance must be sealed (no new fields)");
