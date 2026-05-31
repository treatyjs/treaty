# Treaty

| Feature/Tool                        | Analog                                                         | Treaty                                                     |
| ----------------------------------- | -------------------------------------------------------------- | ---------------------------------------------------------- |
| Vite support                        | ✅                                                             | ✅                                                         |
| First class bun support             | ❌                                                             | ✅                                                         |
| Node Support                        | ✅                                                             | ❌                                                         |
| SSR                                 | ✅                                                             | ✅                                                         |
| SSG                                 | ✅                                                             | 🏗️ (work underway — @treaty/ssg build-time prerender)      |
| File base routing                   | ✅                                                             | ✅                                                         |
| Flexible File routing               | ❌                                                             | ✅                                                         |
| Custom Authoring - functional       | ✅ (.analog - svelte inspired)                                 | ✅ (.treaty - custom & astro inspired)                     |
| Authoring - Class                   | ✅ (standard angular )                                         | ✅ (Enhanced Angular authoring )                           |
| deconstruction of objects to inputs | ❌                                                             | ✅ Supports Singal objects                                 |
| Authoring to Ivy                    | ❌ (transpiles into Angular class for angular NGTCC to handle) | ✅ (render3 direct-to-Ivy compile)                         |
| JSX authoring (selectorless)        | ❌                                                             | ✅ (.tsx/.tjsx, selectorless + signal-aware, @treaty/jsx)  |
| .treaty SFC authoring               | ✅ (.analog SFC)                                               | ✅ (.treaty single-file component — markup + script + styles) |
| Strongly typed HTTP Client          | ❌                                                             | ✅ [@treaty/httpclient](https://jsr.io/@treaty/httpclient) |
| Server side function in component   | ❌                                                             | ✅ (in-component server{} → server module; axum / Elysia / Express plugins) |
| function chunking                   | ❌                                                             | 🏗️ (work underway — server-fn extraction + code-split)     |
| First class Module federation       | ❌                                                             | ✅ (auto zero-config MF + @module-federation/enhanced; route-as-remote; partial deploy/rollback) |
| Zoneless first                      | ❌                                                             | ✅                                                         |
| Build to deploy                     | ❌                                                             | 🏗️ (work underway — @treaty/deploy artifact + pluggable cloud target) |
| Bundler plugins                     | Vite                                                          | ✅ (Vite / Rspack / Rsbuild / Rslib)                       |
| Treaty CLI                          | ❌                                                             | ✅ (treaty dev / build / generate, no angular.json)        |
| Angular lib auto-migration          | ❌                                                             | ✅ (ngx-maintenance — deterministic no-AI bot)             |
| Bun Test                            | ❌                                                             | ✅                                                         |
| [Vitest](https://vitest.dev/)       | ✅                                                             | ❌                                                         |
| Tooling                             | tsc / ESLint                                                  | tsgo + oxlint                                              |
| compiler                            | Angular CLI                                                    | Enhanced with Rust (render3 + oxc; ~55% of the runnable Angular compliance suite and climbing) |
