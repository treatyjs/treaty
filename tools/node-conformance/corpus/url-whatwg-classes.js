// CONFORMANCE: skip — the WHATWG URL/URLSearchParams classes are not reachable in the harness
//
// The runtime installs `URL`/`URLSearchParams` as lazy global accessors (not as named exports of
// node:url, which exposes only the legacy functional parse()/format()/fileURLToPath() API — those
// are covered by url-parse-format.js). Like the other lazy globals, the constructors are not
// triggered inside the harness's indirect-eval scope and are not on globalThis, so `new URL(...)`
// throws "not a constructor". Re-enable once the constructible WHATWG classes are reachable.
const u = new URL("https://example.com:8080/a/b?q=1&q=2#frag");
if (u.hostname !== "example.com") throw new Error("hostname");
if (u.port !== "8080") throw new Error("port");
if (u.pathname !== "/a/b") throw new Error("pathname");
if (u.hash !== "#frag") throw new Error("hash");

const params = new URLSearchParams("a=1&b=2");
if (params.get("a") !== "1") throw new Error("searchParams get");
params.append("a", "3");
if (params.getAll("a").join(",") !== "1,3") throw new Error("searchParams append/getAll");
