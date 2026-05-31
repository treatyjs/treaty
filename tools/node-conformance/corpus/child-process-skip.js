// CONFORMANCE: skip — node:child_process (spawn/exec/fork) is not implemented in the Treaty runtime
//
// Process spawning depends on host process management the runtime does not expose yet. This test is
// also matched centrally by the `child-process-*` glob in known-unsupported.json, so it is recorded
// SKIP whether or not this in-file directive is present. Re-enable once node:child_process lands.
const cp = require("node:child_process");
const out = cp.execSync("echo hi").toString("utf8").trim();
if (out !== "hi") throw new Error("execSync output");
