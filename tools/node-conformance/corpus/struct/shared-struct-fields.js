// CONFORMANCE: skip — struct: unimplemented (TC39 shared structs, Stage 2; Node --harmony-struct)
//
// A `shared struct` is the cross-agent, fixed-layout variant: it has a mandatory `null` prototype, no
// instance methods (data only), and its fields may reference ONLY primitives or other shared
// structs/shared arrays — never an unshared object (the one-way reference rule: "no references from
// shared objects to non-shared objects"). Instances are communicated to other agents via postMessage
// WITHOUT copying (the receiver gets a handle to the same shared object). This is NOT polyfillable —
// it needs a real shared heap + tear-free shared fields, which the Treaty/Nova runtime does not have.
// Intentionally skipped (no shared-heap runtime; no M3 swc recognizer / M4 lowering). Delete the
// directive + the known-unsupported.json entry once a shared-struct runtime + lowering land. The body
// touches the unimplemented surface directly (Node only parses `shared struct` under --harmony-struct).

shared struct SharedBox {
  x;
  y;
}

const s = new SharedBox();
s.x = 1;
s.y = 2;
if (s.x !== 1 || s.y !== 2) throw new Error("shared-struct field read must see exactly one prior write");

// Default field access is unordered (racy) but never tears; sequentially-consistent access uses the
// Atomics overloads that take a struct + field name instead of a TypedArray + index.
if (Atomics.load(s, "x") !== 1) throw new Error("Atomics.load(struct, field) must be sequentially consistent");
Atomics.store(s, "y", 7);
if (s.y !== 7) throw new Error("Atomics.store(struct, field, v) must publish the write");
