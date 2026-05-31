# Treaty

| Feature/Tool                        | Analog                                                         | Treaty                                                     |
| ----------------------------------- | -------------------------------------------------------------- | ---------------------------------------------------------- |
| Vite support                        | ✅                                                             | ✅                                                         |
| First class bun support             | ❌                                                             | ✅                                                         |
| Node Support                        | ✅                                                             | ✅ (Node-compatible Rust runtime on Nova — require + node: builtins, event loop, oxc_resolver) |
| SSR                                 | ✅                                                             | ✅                                                         |
| SSG                                 | ✅                                                             | ✅ (treaty_ssg Rust core — getStaticPaths, head/SEO, hydration manifest, sitemap/robots) |
| File base routing                   | ✅                                                             | ✅ (treaty_file_routing Rust crate)                        |
| Flexible File routing               | ❌                                                             | ✅ (configurable routes/ + api/ conventions)               |
| Custom Authoring - functional       | ✅ (.analog - svelte inspired)                                 | ✅ (.treaty - custom & astro inspired)                     |
| Authoring - Class                   | ✅ (standard angular )                                         | ✅ (Enhanced Angular authoring )                           |
| deconstruction of objects to inputs | ❌                                                             | ✅ Supports Singal objects                                 |
| Authoring to Ivy                    | ❌ (transpiles into Angular class for angular NGTCC to handle) | ✅ (render3 direct-to-Ivy compile)                         |
| JSX authoring (selectorless)        | ❌                                                             | ✅ (.tsx/.tjsx, selectorless + signal-aware + full TS-expression support, @treaty/jsx) |
| .treaty SFC authoring               | ✅ (.analog SFC)                                               | ✅ (.treaty single-file component — markup + script + styles) |
| Source maps                         | ✅                                                             | ✅ (v3 maps end-to-end for every authoring format: .treaty / .tsx / .tjsx / .ts; server-fn bodies redacted from client maps) |
| Strongly typed HTTP Client          | ❌                                                             | ✅ [@treaty/httpclient](https://jsr.io/@treaty/httpclient) |
| Server side function in component   | ❌                                                             | ✅ (in-component server{} → server module; axum / Elysia / Express plugins) |
| function chunking                   | ❌                                                             | ✅ (per-server-fn chunks + client bindings + manifest across Vite / Rspack / Rsbuild / Rslib; bodies never enter the client graph) |
| First class Module federation       | ❌                                                             | ✅ (auto zero-config MF + @module-federation/enhanced; route-as-remote; toggle + standalone-config eject; partial deploy/rollback) |
| Zoneless first                      | ❌                                                             | ✅                                                         |
| Build to deploy                     | ❌                                                             | 🏗️ (work underway — @treaty/deploy artifact + pluggable cloud target) |
| Bundler plugins                     | Vite                                                          | ✅ (Vite / Rspack / Rsbuild / Rslib)                       |
| Treaty CLI                          | ❌                                                             | ✅ (treaty dev / build / generate, no angular.json)        |
| VS Code extension + LSP             | ❌                                                             | ✅ (full extension + TextMate grammars + Angular-file LSP support) |
| Angular lib auto-migration          | ❌                                                             | ✅ (ngx-maintenance — deterministic no-AI bot)             |
| Bun Test                            | ❌                                                             | ✅                                                         |
| [Vitest](https://vitest.dev/)       | ✅                                                             | ❌                                                         |
| Tooling                             | tsc / ESLint                                                  | ✅ (tsgo + oxlint — no tsc anywhere)                       |
| compiler                            | Angular CLI                                                    | Enhanced with Rust (render3 + oxc; direct-to-Ivy; ~90/98 of the runnable Angular compliance suite — 91.8% — and climbing) |
