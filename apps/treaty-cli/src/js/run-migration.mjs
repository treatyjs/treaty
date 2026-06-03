// Treaty's migration driver: run a REAL @angular-devkit migration collection via
// the same NodeWorkflow the Angular CLI's `ng update` uses, 1:1.
//
// argv: <projectRoot> <resolveRoot> <collection> <schematic> <dry|write>
//       -- [passthrough --opt value ...]
//
// `collection` is either an absolute path to a `migrations.json` (the form
// `ng update` derives from a package's `ng-update.migrations` field) or a
// resolvable collection name. `schematic` is the migration name to run.

import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const argv = process.argv.slice(2);
const sep = argv.indexOf('--');
const head = sep === -1 ? argv : argv.slice(0, sep);
const passthrough = sep === -1 ? [] : argv.slice(sep + 1);
const [projectRoot, resolveRoot, collection, schematic, dry] = head;

const require = createRequire(pathToFileURL(resolveRoot.replace(/[\\/]*$/, '/')));

let NodeWorkflow;
try {
  ({ NodeWorkflow } = require('@angular-devkit/schematics/tools'));
} catch (e) {
  console.error('ERR cannot load @angular-devkit/schematics from ' + resolveRoot + ': ' + (e && e.message));
  process.exit(2);
}

function parseOptions(tokens) {
  const out = {};
  for (let i = 0; i < tokens.length; i++) {
    let tok = tokens[i];
    if (!tok.startsWith('--')) continue;
    tok = tok.slice(2);
    const eq = tok.indexOf('=');
    let key, value;
    if (eq !== -1) {
      key = tok.slice(0, eq);
      value = tok.slice(eq + 1);
    } else {
      key = tok;
      const next = tokens[i + 1];
      if (next !== undefined && !next.startsWith('--')) { value = next; i++; }
      else value = true;
    }
    out[key.replace(/-([a-z])/g, (_, c) => c.toUpperCase())] =
      value === 'true' ? true : value === 'false' ? false : value;
  }
  return out;
}

const options = parseOptions(passthrough);

const workflow = new NodeWorkflow(projectRoot, {
  dryRun: dry === 'dry',
  resolvePaths: [projectRoot, resolveRoot, process.cwd()],
  // Migrations run trusted, package-authored schematics; the devkit's own
  // `ng update` runs them with private schematics allowed and lenient schema.
  schemaValidation: false,
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

if (!schematic) {
  console.error('ERR a migration name is required (collection: ' + collection + ')');
  process.exit(1);
}

try {
  await workflow
    .execute({
      collection,
      schematic,
      options,
      allowPrivate: true,
      debug: false,
    })
    .toPromise();
  console.log(dry === 'dry' ? 'OK (dry run — no files written)' : 'OK');
} catch (e) {
  const msg = e && e.message ? e.message : String(e);
  console.error('ERR ' + msg);
  process.exit(1);
}
