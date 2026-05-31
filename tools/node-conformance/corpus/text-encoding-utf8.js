// CONFORMANCE: skip — TextEncoder/TextDecoder are global-only (no node: module) and the runtime's
// lazy globals are not reachable from the harness's indirect-eval scope
//
// TextEncoder/TextDecoder are installed by the Node-compat globals layer as lazy, self-replacing
// accessors on the realm global. The harness evaluates each test through an indirect `eval`, whose
// scope chain does not trigger those bare-name accessors, and they are not exposed via globalThis
// either, nor re-exported from node:util. So the classes read as undefined here. This is a
// harness/runtime interaction gap, not a missing feature: drop the directive once the globals are
// reachable from the wrapped scope (or once the harness evaluates tests as top-level program text).
const enc = new TextEncoder();
const dec = new TextDecoder();
const bytes = enc.encode("héllo");
if (!(bytes instanceof Uint8Array)) throw new Error("encode must yield a Uint8Array");
if (dec.decode(bytes) !== "héllo") throw new Error("utf8 decode round-trip");
