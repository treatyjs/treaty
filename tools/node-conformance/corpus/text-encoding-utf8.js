// TextEncoder/TextDecoder are installed by the Node-compat globals layer as lazy, self-replacing
// accessors on the realm global, and are now reachable from the harness's evaluation scope.
const enc = new TextEncoder();
const dec = new TextDecoder();
const bytes = enc.encode("héllo");
if (!(bytes instanceof Uint8Array)) throw new Error("encode must yield a Uint8Array");
if (dec.decode(bytes) !== "héllo") throw new Error("utf8 decode round-trip");
