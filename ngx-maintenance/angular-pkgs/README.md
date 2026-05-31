# angular-pkgs

Staging area for maintained forks of taken-over Angular libraries, BEFORE each
is split into its own standalone repository.

When the takeover policy (`@ngx-maintenance/takeover`) fires for a library, the
bot forks it, runs the deterministic migration chain
(`@ngx-maintenance/migration-engine`), and stages the result here under
`angular-pkgs/<name>/`. Each staged fork:

- is published under the `@ngx-maintenance/<name>` npm scope,
- carries the mandatory compatibility-only warning banner, and
- is then promoted to its OWN repo (the tooling stays in this Turborepo).

This directory is intentionally empty in the scaffold: forks are added by the
takeover pipeline at runtime, never hand-authored.
