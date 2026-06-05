// CONFORMANCE: skip — struct: unimplemented (TC39 Atomics.Mutex/Condition, Stage 2; Node --harmony-struct)
//
// The structs umbrella ships high-level sync primitives alongside shared structs: `Atomics.Mutex`
// (a non-recursive mutex — `Atomics.Mutex.lock(m)` blocks and returns/charges an UnlockToken that
// supports `using`/`[Symbol.dispose]`) and `Atomics.Condition`
// (`Atomics.Condition.wait(cv, unlockToken)` / `notify(cv[, count])`). They coordinate agents that
// share `shared struct` / `SharedArray` data. NOT polyfillable — they require a real futex-backed
// mutex/condition over a shared heap. Intentionally skipped (no shared-memory sync runtime yet).
// Delete the directive + the known-unsupported.json entry once these land. The body touches the
// unimplemented surface directly.

const mutex = new Atomics.Mutex();
const cond = new Atomics.Condition();

const token = Atomics.Mutex.lock(mutex);
if (!token.locked) throw new Error("Atomics.Mutex.lock must return a locked UnlockToken");
// notify with no waiters is a no-op but must be callable while holding the lock.
Atomics.Condition.notify(cond, 0);
token.unlock();
if (token.locked) throw new Error("UnlockToken.unlock() must release the mutex");
