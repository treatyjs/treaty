// CONFORMANCE: skip — struct: unimplemented (TC39 SharedArray, Stage 2; Node --harmony-struct)
//
// `SharedArray` is the fixed-length shared array companion of the structs proposal: `new
// SharedArray(n)` allocates a shared, fixed-length array with a read-only `length`, whose elements
// (like shared-struct fields) hold only primitives or other shared values, are tear-free, and may be
// accessed sequentially-consistently via the Atomics(struct/array, index) overloads. Like shared
// structs it is NOT polyfillable — it requires a real shared heap. Intentionally skipped (no
// shared-memory-object runtime yet). Delete the directive + the known-unsupported.json entry once a
// SharedArray runtime lands. The body touches the unimplemented surface directly.

const arr = new SharedArray(4);
if (arr.length !== 4) throw new Error("SharedArray length must equal the requested fixed length");

arr[0] = 11;
Atomics.store(arr, 1, 22);
if (arr[0] !== 11) throw new Error("SharedArray element read must see exactly one prior write");
if (Atomics.load(arr, 1) !== 22) throw new Error("Atomics.load(sharedArray, index) must be consistent");
