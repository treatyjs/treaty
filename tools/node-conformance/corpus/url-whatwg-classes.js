// The runtime installs `URL`/`URLSearchParams` as lazy global accessors (the constructible WHATWG
// classes), distinct from node:url's legacy functional parse()/format()/fileURLToPath() API covered
// by url-parse-format.js. The constructors are now reachable from the harness's evaluation scope.
const u = new URL("https://example.com:8080/a/b?q=1&q=2#frag");
if (u.hostname !== "example.com") throw new Error("hostname");
if (u.port !== "8080") throw new Error("port");
if (u.pathname !== "/a/b") throw new Error("pathname");
if (u.hash !== "#frag") throw new Error("hash");

const params = new URLSearchParams("a=1&b=2");
if (params.get("a") !== "1") throw new Error("searchParams get");
params.append("a", "3");
if (params.getAll("a").join(",") !== "1,3") throw new Error("searchParams append/getAll");
