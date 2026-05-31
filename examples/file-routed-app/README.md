# file-routed-app

A realistic example directory tree that exercises the **full** file-routing
convention implemented by the `treaty_file_routing` crate (`libs/file-routing`).

The point of this app is its *shape*: a real `routes/` + `api/` tree on disk
that the crate's `generate_routing(config, &dyn DirTree)` pipeline is pointed at
end-to-end (via a filesystem-backed `DirTree`) to produce Angular lazy routes,
Module Federation remotes, and a server-endpoint manifest.

## Running file-based routing

`src/generated/routes.ts` is **generated** from the on-disk `routes/` + `api/`
tree by the real `treaty_file_routing` engine — it is never hand-written. One
command produces it:

```sh
# From this directory (examples/file-routed-app):
npm run generate-routes        # alias: npm run routes
```

That runs `scripts/generate-routes.mjs`, which:

1. `cargo build`s the detached crate's CLI
   (`cargo build --manifest-path libs/file-routing/Cargo.toml`, a no-op once
   compiled),
2. runs the binary over **this app's own project root** twice —
   `treaty-file-routing . --style colon --emit ts` for the Angular `Routes`
   array + `federationRemotes`, and `--emit json` to lift the api/ endpoint
   manifest — and
3. writes the combined module to `src/generated/routes.ts` (`routes`,
   `federationRemotes`, and `apiEndpoints` exports).

`vite.config.ts` re-runs the same script on `buildStart`, so the generated
routes are always current with the directory tree. To invoke the engine
directly instead of through the npm script (run from the repo root):

```sh
# Full GeneratedRouting (routes + remotes + endpoints) as JSON:
cargo run --manifest-path libs/file-routing/Cargo.toml --bin treaty-file-routing -- \
  examples/file-routed-app --style colon

# Just the ready-to-import Angular route module (lazy loadComponent() + remotes):
cargo run --manifest-path libs/file-routing/Cargo.toml --bin treaty-file-routing -- \
  examples/file-routed-app --style colon --emit ts
```

### How `routes/` + `api/` map to the output

Each entry below is asserted by the end-to-end test
(`npm run test:e2e`, `test/routing.e2e.mjs`), which regenerates the module and
checks the result against the tree — proving the generation is real and correct:

| On disk | Generated route `path` |
| --- | --- |
| `routes/layout.treaty` | `""` (root layout parent) |
| `routes/index.treaty` | `""` (landing, child of root layout) |
| `routes/(marketing)/index.treaty` | `""` (group name stripped) |
| `routes/(marketing)/about.tjsx` | `"about"` (group name stripped) |
| `routes/blog/layout.treaty` | `"blog"` (nested layout parent) |
| `routes/blog/index.treaty` | `""` under `blog` |
| `routes/blog/[slug]/index.treaty` | `":slug"` |
| `routes/blog/[...path]/index.treaty` | `":...path"` (catch-all directory) |
| `routes/docs/[category]/[page]/index.tjsx` | `"docs/:category/:page"` |
| `routes/not-found.treaty` | `"**"` (wildcard) |

| On disk | Generated `apiEndpoints[].path` |
| --- | --- |
| `api/index.ts` | `/` |
| `api/health/index.ts` | `/health` |
| `api/posts/index.ts` | `/posts` |
| `api/posts/[id]/index.ts` | `/posts/:id` (param `id`) |

The generator uses `--style colon`, so dynamic segments are the Angular-router
native `:param` form and `provideRouter(routes)` consumes the module as-is. The
tables in **Expected output** below are the **verified** colon-style output the
E2E test asserts against. The pretty inspector example
(`cargo run --example real_dir_tree -- examples/file-routed-app`) prints the same
data as an indented summary.

## The convention

Run with the default config:

| Setting | Default | Meaning |
| --- | --- | --- |
| `routes_dir` | `routes` | directory of page/layout components |
| `api_dir` | `api` | directory of server-function handlers |
| `index_file_names` | `index`, `page` | a directory's own route/endpoint |
| `layout_file_name` | `layout` | makes its directory a parent route wrapping children |
| `not_found_file_name` | `not-found` | lowers to an Angular `**` wildcard route |
| `route_extensions` | `.treaty`, `.tjsx`, `.tsx`, `.ts` | recognised route component files |
| `api_extensions` | `.treaty`, `.ts` | recognised api handler files (note: **no `.tsx`/`.tjsx`**) |
| `dynamic_segment_style` | Bracket (`[id]`) | both `[id]` and `:id` parse on input; Bracket is the crate default, but this app generates with `--style colon` for Angular-native `:id` output |
| `federation` | on | every lazy route + layout boundary is emitted as a `FederationRemote` |

Naming rules applied to directory and file base names:

- **Static** — `blog`, `about` → a literal path segment.
- **Dynamic** — `[slug]` (or `:slug`) → a route parameter. This app generates
  with `--style colon`, so the *route* path renders `:slug`; *api* paths always
  render dynamic segments as `:slug` regardless of style.
- **Catch-all** — `[...path]` (or `:...path`) → a rest parameter. In the **api**
  manifest this becomes `*path`. In **routes** a catch-all *directory* lowers to
  a normal dynamic path segment (`:...path` under colon style); only a
  `not-found` file produces the `**` wildcard.
- **Route group** — `(marketing)` → organises files in a folder **without
  contributing a URL segment** (Next.js / Analog semantics). The parenthesised
  name is stripped from the generated route `path`: `(marketing)/about` lowers to
  `about`, and `(marketing)/index` lowers to `""` — the group's index *is* the
  parent's index URL. The group still owns and nests its children (and, if it has
  a `layout`, it mounts that layout at the parent path). When a group index and
  the parent index both resolve to the same URL (`""`), the path is the same by
  design; they are disambiguated only at the federation-remote layer (see below).
- **Index/page** — `index.*` / `page.*` → the directory's own route at `""`
  (under a layout) or the directory's joined path (flattened).

### Layout vs. flatten

A directory **with** a `layout` file becomes one parent `AngularRoute` at the
directory's path; its index, pages, child dirs, and `not-found` nest as
`children` with paths relative to the layout. A directory **without** a layout
is *flattened*: its contents are emitted into the parent's list with the
directory segment prefixed onto each path. This app shows both: the root and
`blog/` use layouts (nesting), while `(marketing)/` and `docs/.../` flatten.
(A route group with **no** layout, like `(marketing)/`, flattens with an *empty*
segment, so only the group's children — not the group name — reach the URL.)

## Directory tree

```
examples/file-routed-app/
├── README.md
├── routes/
│   ├── layout.treaty            # root layout  -> parent route at ""
│   ├── index.treaty             # landing page -> "" (child of root layout)
│   ├── not-found.treaty         # 404          -> "**" wildcard
│   ├── (marketing)/             # route group  (paren segment STRIPPED from URL)
│   │   ├── index.treaty         #   -> ""        (group index = parent index URL)
│   │   └── about.tjsx           #   -> "about"   (JSX authoring; group name gone)
│   ├── blog/
│   │   ├── layout.treaty        # blog layout  -> nested parent route at "blog"
│   │   ├── index.treaty         #   -> "" under blog  (URL /blog)
│   │   ├── [slug]/index.treaty  # dynamic post -> ":slug" under blog
│   │   └── [...path]/index.treaty  # catch-all -> ":...path" under blog
│   └── docs/
│       └── [category]/
│           └── [page]/
│               └── index.tjsx   # deep nested dynamic -> "docs/:category/:page"
└── api/
    ├── index.ts                 # root handler        -> "/"
    ├── health/
    │   └── index.ts             # nested handler      -> "/health"
    └── posts/
        ├── index.ts             # collection handler  -> "/posts"
        └── [id]/index.ts        # dynamic handler     -> "/posts/:id"
```

## Expected output (default config)

### Angular routes

A single top-level route (the root layout) wraps everything. `[depth]` shows the
nesting; emission order is deterministic (children sorted lexicographically by
the scanner, then lowered).

| Route path | Kind | Component / layout file |
| --- | --- | --- |
| `""` | layout | `routes/layout.treaty` |
| ` ""` | leaf | `routes/index.treaty` |
| ` ""` | leaf | `routes/(marketing)/index.treaty` |
| ` about` | leaf | `routes/(marketing)/about.tjsx` |
| ` blog` | layout | `routes/blog/layout.treaty` |
| `  ""` | leaf | `routes/blog/index.treaty` |
| `  :...path` | leaf | `routes/blog/[...path]/index.treaty` |
| `  :slug` | leaf | `routes/blog/[slug]/index.treaty` |
| ` docs/:category/:page` | leaf | `routes/docs/[category]/[page]/index.tjsx` |
| ` **` | leaf (wildcard) | `routes/not-found.treaty` |

Leading spaces indicate child depth under the root layout / blog layout.

Note that the `(marketing)` route group contributes **no** URL segment: its
`index` lowers to `""` (sharing the root index's URL) and its `about` page lowers
to `about`. The two `""` siblings under the root layout are the documented
group-index/parent-index collision — the URL is `""` for both by design, and the
authored intent (root landing vs. marketing landing) is the developer's to
reconcile; their *remote names*, however, are made unique (below).

### Federation remotes

Depth-first, route-order. Every lazy route and every layout boundary yields one
remote (`exposedModule` is always `./Route`). Module Federation requires every
remote name to be **globally unique**, so names are derived in two steps:

1. **Base slug** from the (group-stripped) route path — the empty path slugifies
   to `root`, `**` to `not-found`, otherwise lowercased/hyphenated segments.
2. **Disambiguation** when a base is already taken. The *first* route to claim a
   base keeps it bare; later collisions append a hint derived from the entry
   file — its owning directory name for a nested `index`/`page`, or the file stem
   otherwise — and, only if that still collides, a numeric `-2`, `-3`, … suffix.

So the four routes that all slugify to `root` (root layout, root index, the
`(marketing)` index, and the blog index) become `root`, `root-index`,
`root-marketing`, and `root-blog`. The `route_path` column is always the **real,
group-stripped** path (so `(marketing)` never appears in it).

| Remote name | Route path | Entry file |
| --- | --- | --- |
| `root` | `""` | `routes/layout.treaty` |
| `root-index` | `""` | `routes/index.treaty` |
| `root-marketing` | `""` | `routes/(marketing)/index.treaty` |
| `about` | `about` | `routes/(marketing)/about.tjsx` |
| `blog` | `blog` | `routes/blog/layout.treaty` |
| `root-blog` | `""` | `routes/blog/index.treaty` |
| `path` | `:...path` | `routes/blog/[...path]/index.treaty` |
| `slug` | `:slug` | `routes/blog/[slug]/index.treaty` |
| `docs-category-page` | `docs/:category/:page` | `routes/docs/[category]/[page]/index.tjsx` |
| `not-found` | `**` | `routes/not-found.treaty` |

### API endpoints

Sorted by path. API dynamic segments always render `:param`.

| Endpoint path | Handler file | Params |
| --- | --- | --- |
| `/` | `api/index.ts` | `[]` |
| `/health` | `api/health/index.ts` | `[]` |
| `/posts` | `api/posts/index.ts` | `[]` |
| `/posts/:id` | `api/posts/[id]/index.ts` | `["id"]` |

(The richer `build_manifest` view also infers HTTP methods from the file base
name. These handlers carry no method hint in their names, so each answers all
methods — the backend plugin narrows them.)
