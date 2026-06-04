// CONFORMANCE: skip — struct: unimplemented (TC39 JavaScript Structs, Stage 2; Node --harmony-struct)
//
// A `struct` may `extends` only another `struct`. All declared fields — including the superclass's —
// are pre-initialized to `undefined` before any user code observes the instance, and the subclass
// constructor must `super(...)` exactly like a class. This case is intentionally skipped (no runtime
// struct support yet, no M4 lowering). Delete the directive + the known-unsupported.json entry once
// unshared-struct lowering lands. The body touches the unimplemented surface directly.

struct Point {
  x;
  constructor(x) {
    this.x = x;
  }
}

struct Point3 extends Point {
  z;
  constructor(x, z) {
    super(x);
    this.z = z;
  }
  norm() {
    return this.x * this.x + this.z * this.z;
  }
}

const p = new Point3(3, 4);
if (p.x !== 3 || p.z !== 4) throw new Error("inherited + own struct fields must initialize");
if (p.norm() !== 25) throw new Error("struct subclass method must read both layouts");
