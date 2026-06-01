/**
 * Aggregate runner for @treaty/deploy's smoke suites, so a single `moon run
 * treaty-deploy:test` exercises every suite in this package.
 *
 * Each suite runs its own checks at import time and only calls process.exit on
 * FAILURE (a passing suite logs "SMOKE TEST PASSED" and returns), so importing
 * them in sequence runs all of them; the first failing suite aborts with a
 * non-zero exit, which is exactly the gate behavior we want.
 *
 * Run: node libs/treaty/deploy/test/all.smoke.mjs
 */

await import('./deploy.smoke.mjs')
await import('./http-target.smoke.mjs')
await import('./remote.smoke.mjs')

console.log('\nALL @treaty/deploy SMOKE SUITES PASSED')
