# file-routed-app

A realistic example directory tree that exercises the **full** file-routing
convention implemented by the `treaty_file_routing` crate (`libs/file-routing`).

These are example **source files** — there is no build step. The point of this
app is its *shape*: a real `routes/` + `api/` tree on disk that the crate's
`generate_routing(config, &dyn DirTree)` pipeline can be pointed at end-to-end
(via a filesystem-backed `DirTree`) to produce Angular lazy routes, Module
Federation remotes, and a server-endpoint manifest.

You can run the pipeline against this exact tree:

```sh
cargo run --manifest-path libs/file-routing/Cargo.toml \
  --example real_dir_tree -- examples/file-routed-app
```

The tables at the bottom of this file are the **verified** output of that run
under the default `FileRoutingConfig` (Bracket dynamic style, federation on),
so an E2E test can assert against them.

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
| `dynamic_segment_style` | Bracket (`[id]`) | both `[id]` and `:id` parse on input; Bracket is the canonical output spelling |
| `federation` | on | every lazy route + layout boundary is emitted as a `FederationRemote` |

Naming rules applied to directory and file base names:

- **Static** — `blog`, `about` → a literal path segment.
- **Dynamic** — `[slug]` (or `:slug`) → a route parameter. Under the default
  Bracket style the *route* path keeps the `[slug]` spelling; *api* paths always
  render dynamic segments as `:slug`.
- **Catch-all** — `[...path]` (or `:...path`) → a rest parameter. In the **api**
  manifest this becomes `*path`. In **routes** a catch-all *directory* lowers to
  a normal dynamic path segment (`[...path]`); only a `not-found` file produces
  the `**` wildcard.
- **Route group** — `(marketing)` → organises files in a folder. The scanner
  records the group; the current route lowering treats the parenthesised name as
  the path segment (so it appears in the path), which a later flattening pass can
  strip.
- **Index/page** — `index.*` / `page.*` → the directory's own route at `""`
  (under a layout) or the directory's joined path (flattened).

### Layout vs. flatten

A directory **with** a `layout` file becomes one parent `AngularRoute` at the
directory's path; its index, pages, child dirs, and `not-found` nest as
`children` with paths relative to the layout. A directory **without** a layout
is *flattened*: its contents are emitted into the parent's list with the
directory segment prefixed onto each path. This app shows both: the root and
`blog/` use layouts (nesting), while `(marketing)/` and `docs/.../` flatten.

## Directory tree

```
examples/file-routed-app/
├── README.md
├── routes/
│   ├── layout.treaty            # root layout  -> parent route at ""
│   ├── index.treaty             # landing page -> "" (child of root layout)
│   ├── not-found.treaty         # 404          -> "**" wildcard
│   ├── (marketing)/             # route group  (paren segment kept by lowering)
│   │   ├── index.treaty         #   -> "(marketing)"
│   │   └── about.tjsx           #   -> "(marketing)/about"   (JSX authoring)
│   ├── blog/
│   │   ├── layout.treaty        # blog layout  -> nested parent route at "blog"
│   │   ├── index.treaty         #   -> "" under blog  (URL /blog)
│   │   ├── [slug]/index.treaty  # dynamic post -> "[slug]" under blog
│   │   └── [...path]/index.treaty  # catch-all -> "[...path]" under blog
│   └── docs/
│       └── [category]/
│           └── [page]/
│               └── index.tjsx   # deep nested dynamic -> "docs/[category]/[page]"
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
| ` (marketing)` | leaf | `routes/(marketing)/index.treaty` |
| ` (marketing)/about` | leaf | `routes/(marketing)/about.tjsx` |
| ` blog` | layout | `routes/blog/layout.treaty` |
| `  ""` | leaf | `routes/blog/index.treaty` |
| `  [...path]` | leaf | `routes/blog/[...path]/index.treaty` |
| `  [slug]` | leaf | `routes/blog/[slug]/index.treaty` |
| ` docs/[category]/[page]` | leaf | `routes/docs/[category]/[page]/index.tjsx` |
| ` **` | leaf (wildcard) | `routes/not-found.treaty` |

Leading spaces indicate child depth under the root layout / blog layout.

### Federation remotes

Depth-first, route-order. Every lazy route and every layout boundary yields one
remote (`exposedModule` is always `./Route`). Remote names are slugified route
paths; the empty path slugifies to `root` and `**` to `not-found` — so the root
layout, the root index, and the blog index all share the name `root` (paths
differ; names are intentionally path-derived and not required unique).

| Remote name | Route path | Entry file |
| --- | --- | --- |
| `root` | `""` | `routes/layout.treaty` |
| `root` | `""` | `routes/index.treaty` |
| `marketing` | `(marketing)` | `routes/(marketing)/index.treaty` |
| `marketing-about` | `(marketing)/about` | `routes/(marketing)/about.tjsx` |
| `blog` | `blog` | `routes/blog/layout.treaty` |
| `root` | `""` | `routes/blog/index.treaty` |
| `path` | `[...path]` | `routes/blog/[...path]/index.treaty` |
| `slug` | `[slug]` | `routes/blog/[slug]/index.treaty` |
| `docs-category-page` | `docs/[category]/[page]` | `routes/docs/[category]/[page]/index.tjsx` |
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
