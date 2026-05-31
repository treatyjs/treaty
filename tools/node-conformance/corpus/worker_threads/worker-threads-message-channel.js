// CONFORMANCE: skip — node:worker_threads is pending (the runtime has no thread/MessagePort transport yet)
//
// This case is intentionally skipped to exercise the inline `CONFORMANCE: skip` path of the harness
// and to keep a single, visible, diffable record of a genuine Node-compat gap. node:worker_threads
// needs a real OS-thread-backed isolate plus a structured-clone MessageChannel between threads; the
// Treaty runtime ships neither yet. Delete this directive (and this file's skip) once worker_threads
// lands. The body below is the test we WILL run at that point; it must never execute while skipped,
// so it deliberately touches the unimplemented surface.
const { Worker, MessageChannel, isMainThread } = require("node:worker_threads");

const { port1, port2 } = new MessageChannel();
port1.postMessage({ hello: "world" });
port2.on("message", (msg) => {
  if (msg.hello !== "world") throw new Error("message channel round-trip failed");
});

if (isMainThread) {
  const worker = new Worker("module.exports = 1;", { eval: true });
  worker.on("exit", (code) => {
    if (code !== 0) throw new Error("worker exited non-zero: " + code);
  });
}
