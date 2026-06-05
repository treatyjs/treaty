// Treaty's schematics driver: run the REAL @angular-devkit/schematics NodeWorkflow
// against a collection + schematic, 1:1 with the Angular CLI. Treaty spawns this
// with the genuine devkit resolved from the project's (or repo's) node_modules.
//
// argv: <projectRoot> <resolveRoot> <collection> <schematic> <dry|write>
//       <force|no-force> <name> -- [passthrough --opt value ...]
//
// Output: one line per filesystem action (CREATE/UPDATE/DELETE/RENAME <path>),
// then OK, or `ERR <message>` + non-zero exit on failure — a stable, parseable
// surface Treaty streams straight through to the user.

import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const argv = process.argv.slice(2);
const sep = argv.indexOf('--');
const head = sep === -1 ? argv : argv.slice(0, sep);
const passthrough = sep === -1 ? [] : argv.slice(sep + 1);
const [projectRoot, resolveRoot, collection, schematic, dry, force, name] = head;

const require = createRequire(pathToFileURL(resolveRoot.replace(/[\\/]*$/, '/') ));

let NodeWorkflow;
try {
  ({ NodeWorkflow } = require('@angular-devkit/schematics/tools'));
} catch (e) {
  console.error('ERR cannot load @angular-devkit/schematics from ' + resolveRoot + ': ' + (e && e.message));
  process.exit(2);
}

// Parse the passthrough `--key value` / `--flag` / `--key=value` into an options
// object the schematic receives 1:1. Bare `--flag` becomes boolean true; a value
// that parses as a number/boolean is coerced (schemas expect typed values).
function parseOptions(tokens) {
  const out = {};
  for (let i = 0; i < tokens.length; i++) {
    let tok = tokens[i];
    if (!tok.startsWith('--')) continue;
    tok = tok.slice(2);
    let key, value;
    const eq = tok.indexOf('=');
    if (eq !== -1) {
      key = tok.slice(0, eq);
      value = tok.slice(eq + 1);
    } else {
      key = tok;
      const next = tokens[i + 1];
      if (next !== undefined && !next.startsWith('--')) {
        value = next;
        i++;
      } else {
        value = true;
      }
    }
    out[camel(key)] = coerce(value);
  }
  return out;
}
function camel(s) {
  return s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());
}
function coerce(v) {
  if (v === true) return true;
  if (v === 'true') return true;
  if (v === 'false') return false;
  if (v !== '' && !isNaN(Number(v))) return Number(v);
  return v;
}

const options = parseOptions(passthrough);
if (name) options.name = name;

const workflow = new NodeWorkflow(projectRoot, {
  dryRun: dry === 'dry',
  force: force === 'force',
  // Resolve collections/schematics from the project first, then the fallback
  // resolution root (where a hoisted devkit/collection lives), then cwd.
  resolvePaths: [projectRoot, resolveRoot, process.cwd()],
  schemaValidation: true,
  packageManager: 'npm',
});

workflow.reporter.subscribe((event) => {
  const p = (event.path || '').replace(/^\//, '');
  switch (event.kind) {
    case 'create': console.log('CREATE ' + p); break;
    case 'update': console.log('UPDATE ' + p); break;
    case 'delete': console.log('DELETE ' + p); break;
    case 'rename': console.log('RENAME ' + p + ' -> ' + String(event.to || '').replace(/^\//, '')); break;
    default: break;
  }
});

// Surface schematic lifecycle logs (e.g. "Nothing to be done", warnings).
workflow.lifeCycle.subscribe(() => {});

try {
  await workflow
    .execute({
      collection,
      schematic,
      options,
      allowPrivate: false,
      debug: false,
      logger: undefined,
    })
    .toPromise();
  console.log(dry === 'dry' ? 'OK (dry run — no files written)' : 'OK');
} catch (e) {
  const msg = e && e.message ? e.message : String(e);
  console.error('ERR ' + msg);
  process.exit(1);
}
