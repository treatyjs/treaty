// @ts-check
/**
 * A faithful port of Angular's compliance matcher for PARTIAL goldens: the `expectEmit` /
 * `replaceMacros` / source-mapping-annotation handling in
 * `packages/compiler-cli/test/compliance/test_helpers/{expect_emit,expected_file_macros,
 * sourcemap_helpers,function_checks,di_checks}.ts` at the pinned Angular commit.
 *
 * A partial golden (for example `r3_view_compiler_boundaries/basic.js`) holds only template
 * functions, with `…` / `// ...` ellipses for the parts it does not pin, `$name$` placeholders
 * that bind to whatever identifier the emit uses (and must agree everywhere they recur), and
 * the `__AttributeMarker.X__` / `__SelectorFlags.X__` / `__QueryFlags.X__` macros. Angular
 * matches the golden's tokens IN ORDER against the generated file, each chunk searched forward
 * from the end of the previous one.
 *
 * This module keeps Angular's rules exactly and turns any mismatch into a thrown error. It never
 * reports a pass for input it could not check:
 *   - `ExpectEmitMismatch`: the generated code does not contain the golden (a DIFF).
 *   - `ExpectEmitInvalid`: the golden itself cannot be tokenized (a skip, not a pass).
 *   - `UnsupportedMacro`: an i18n macro, which needs Angular's message serializer (a skip).
 *   - The `// SOURCE:` source-map assertions are NOT checked (Treaty emits no source map for the
 *     partial goldens); `stripSourceMappingAnnotations` removes the annotation and counts them so
 *     the report can say how many assertions were not verified.
 *
 * Run the unit tests with: node --test libs/treaty-ivy/facade/compliance/expect-emit.test.mjs
 */

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/** The generated code does not contain the golden's tokens in order. */
export class ExpectEmitMismatch extends Error {
  /**
   * @param {string} message
   * @param {string} piece  the expected token that could not be found
   */
  constructor(message, piece) {
    super(message);
    this.name = 'ExpectEmitMismatch';
    this.piece = piece;
  }
}

/** The golden cannot be tokenized, so Angular's check cannot even run. */
export class ExpectEmitInvalid extends Error {
  constructor(message) {
    super(message);
    this.name = 'ExpectEmitInvalid';
  }
}

/** An expected-file macro this port does not evaluate (the i18n message macros). */
export class UnsupportedMacro extends Error {
  /** @param {string} macro */
  constructor(macro) {
    super(`unsupported expected-file macro: ${macro}`);
    this.name = 'UnsupportedMacro';
    this.macro = macro;
  }
}

// ---------------------------------------------------------------------------
// Tokenizer (expect_emit.ts `tokenize`).
// ---------------------------------------------------------------------------

const IDENTIFIER = /[A-Za-z_$ɵ][A-Za-z0-9_$]*/;
const COMMENT_START = /\/\*/;
const COMMENT_END = /\*\//;
const OPERATOR =
  /!|\?|%|\*|\/|\^|&&?|\|\|?|\(|\)|\{|\}|\[|\]|:|;|<=?|>=?|={1,3}|!==?|=>|\+\+?|--?|@|,|\.|\.\.\.|\\`|\\'/;
const STRING = /'(\\'|[^'])*'|"(\\"|[^"])*"/;
const INLINE_BACKTICK_STRING = /`(?:[\s\S]|(?:\$\{[^}]*?\}))*?[^\\]`/;
const SINGLE_BACKTICK_STRING = new RegExp('^' + INLINE_BACKTICK_STRING.source);
const BACKTICK_INTERPOLATION = /(\$\{[^}]*\})/;
const NUMBER = /\d+/;

const ELLIPSIS = '…';
const TOKEN = new RegExp(
  `\\s*((${COMMENT_START.source})|(${COMMENT_END.source})|(${IDENTIFIER.source})|(${INLINE_BACKTICK_STRING.source})|(${OPERATOR.source})|(${STRING.source})|${NUMBER.source}|${ELLIPSIS})\\s*`,
  'y',
);

/** A matcher piece: a literal token (string) or a regular expression (identifier / placeholder). */
const SKIP = /(?:.|\n|\r)*?/;

/**
 * Tokenize an expected golden into pieces. `SKIP` marks an ellipsis.
 * @param {string} text
 * @returns {(string | RegExp)[]}
 */
export function tokenize(text) {
  // TOKEN.lastIndex is stateful, so save it and restore it at the end (recursion is possible).
  const lastIndex = TOKEN.lastIndex;
  TOKEN.lastIndex = 0;

  /** @type {RegExpExecArray | null} */
  let match;
  let tokenizedTextEnd = 0;
  let inComment = false;
  /** @type {(string | RegExp)[]} */
  const pieces = [];
  while ((match = TOKEN.exec(text)) !== null) {
    const token = match[1];
    if (token === 'IDENT') {
      pieces.push(IDENTIFIER);
    } else if (token === ELLIPSIS) {
      pieces.push(SKIP);
    } else if ((match = SINGLE_BACKTICK_STRING.exec(token))) {
      if (inComment) {
        // Inside a comment block a backtick is an ordinary token: store it and resume after it.
        pieces.push('`');
        TOKEN.lastIndex = tokenizedTextEnd + 1;
      } else {
        pieces.push(...tokenizeBackTickString(token));
      }
    } else {
      updateCommentState(token);
      pieces.push(token);
    }
    tokenizedTextEnd = TOKEN.lastIndex;
  }

  if (pieces.length === 0 || tokenizedTextEnd < text.length) {
    // The token that could not be found starts right after the last tokenized character.
    const from = tokenizedTextEnd;
    const to = from + 30;
    throw new ExpectEmitInvalid(
      `Invalid test, no token found for "${text[tokenizedTextEnd]}" (context = '${text.slice(from, to)}...')`,
    );
  }
  TOKEN.lastIndex = lastIndex;
  return pieces;

  /** @param {string} token */
  function updateCommentState(token) {
    if (token === '/*') {
      inComment = true;
    } else if (token === '*/') {
      inComment = false;
    }
  }
}

/**
 * Back-ticks are escaped as "\`" so the backslashes are stripped. An interpolation inside the
 * string is tokenized too, because it may hold an identifier that must be matched later.
 * @param {string} str
 * @returns {(string | RegExp)[]}
 */
function tokenizeBackTickString(str) {
  /** @type {(string | RegExp)[]} */
  const pieces = ['`'];
  const backTickPieces = str.slice(1, -1).split(BACKTICK_INTERPOLATION);
  backTickPieces.forEach((backTickPiece) => {
    if (BACKTICK_INTERPOLATION.test(backTickPiece)) {
      pieces.push(...tokenize(backTickPiece));
    } else {
      pieces.push(backTickPiece);
    }
  });
  pieces.push('`');
  return pieces;
}

// ---------------------------------------------------------------------------
// Chunk matcher (expect_emit.ts `buildChunkMatcher`).
// ---------------------------------------------------------------------------

const IDENT_LIKE = /^[a-z][A-Z]/;
const MATCHING_IDENT = /^\$.*\$$/;

/** Angular's `escapeRegExp` (`compiler/src/util.ts`). */
export function escapeRegExp(s) {
  return s.replace(/([.*+?^=!:${}()|[\]\/\\])/g, '\\$1');
}

/**
 * Build the regular expression that matches `pieces`. `$name$` placeholders capture an identifier
 * the first time they appear and must match the same text wherever they recur (`knownGroups`
 * carries the captures of earlier chunks).
 * @param {(string | RegExp)[]} pieces
 * @param {Map<string, string>} knownGroups
 * @param {string} flag
 * @returns {{regexp: RegExp, newGroups: Map<string, number>}}
 */
function buildChunkMatcher(pieces, knownGroups, flag) {
  /** @type {string[]} */
  const results = [];
  let first = true;
  let groupCounter = 0;
  /** @type {Map<string, number>} */
  const newGroups = new Map();

  for (const piece of pieces) {
    if (!first) {
      results.push(`\\s${typeof piece === 'string' && IDENT_LIKE.test(piece) ? '+' : '*'}`);
    }
    first = false;

    if (typeof piece === 'string') {
      if (MATCHING_IDENT.test(piece)) {
        if (knownGroups.has(piece)) {
          results.push(escapeRegExp(/** @type {string} */ (knownGroups.get(piece))));
        } else {
          const matchGroup = newGroups.get(piece);
          if (!matchGroup) {
            results.push('(' + IDENTIFIER.source + ')');
            groupCounter++;
            newGroups.set(piece, groupCounter);
          } else {
            results.push(`\\${matchGroup}`);
          }
        }
      } else {
        results.push(escapeRegExp(piece));
      }
    } else {
      results.push('(?:' + piece.source + ')');
    }
  }
  return { regexp: new RegExp(results.join(''), flag), newGroups };
}

// ---------------------------------------------------------------------------
// expectEmit (expect_emit.ts `expectEmit`).
// ---------------------------------------------------------------------------

/**
 * Check that `source` contains the golden `expected`, token for token and in order. Throws
 * `ExpectEmitMismatch` at the first token that cannot be found, and `ExpectEmitInvalid` when the
 * golden itself cannot be tokenized. Returns normally only when every token was found.
 *
 * @param {string} source       the generated file
 * @param {string} expected     the golden, after macros and source-map annotations were processed
 * @param {string} description  prefixed to any error message
 */
export function expectEmit(source, expected, description) {
  expected = expected
    // turns `// ...` into `…`
    .replace(/\/\/\s*\.\.\./g, ELLIPSIS)
    // remove `// TODO` comment lines
    .replace(/\/\/\s*TODO.*?\n/g, '')
    // remove `// NOTE` comment lines
    .replace(/\/\/\s*NOTE.*?\n/g, '');

  const pieces = tokenize(expected);

  // Group pieces into chunks separated by SKIP.
  /** @type {{pieces: (string | RegExp)[], skipBefore: boolean}[]} */
  const chunks = [];
  /** @type {(string | RegExp)[]} */
  let currentChunk = [];
  let hasSkip = false;
  for (const piece of pieces) {
    if (piece === SKIP) {
      if (currentChunk.length > 0 || hasSkip) {
        chunks.push({ pieces: currentChunk, skipBefore: hasSkip });
      }
      hasSkip = true;
      currentChunk = [];
    } else {
      currentChunk.push(piece);
    }
  }
  if (currentChunk.length > 0 || chunks.length === 0) {
    chunks.push({ pieces: currentChunk, skipBefore: hasSkip });
  }

  /** @type {Map<string, string>} */
  const extractedGroups = new Map();
  let lastIndex = 0;

  for (const chunk of chunks) {
    if (chunk.pieces.length === 0) continue;

    const { regexp, newGroups } = buildChunkMatcher(chunk.pieces, extractedGroups, 'g');
    regexp.lastIndex = lastIndex;
    const m = regexp.exec(source);

    if (!m) {
      // Find the first piece that cannot be matched after the previous chunk, for the report.
      let errLast = lastIndex;
      for (let j = 1; j <= chunk.pieces.length; j++) {
        const subPieces = chunk.pieces.slice(0, j);
        const { regexp: subRegexp } = buildChunkMatcher(subPieces, extractedGroups, 'g');
        subRegexp.lastIndex = lastIndex;
        const subMatch = subRegexp.exec(source);
        const piece = chunk.pieces[j - 1];
        const expectedPiece = piece === IDENTIFIER ? '<IDENT>' : String(pieceText(piece));
        if (!subMatch) {
          const contextLength = 50;
          const fullContext = source.substring(source.lastIndexOf('\n', errLast) + 1, errLast);
          const context =
            fullContext.length > contextLength ? `...${fullContext.slice(-contextLength)}` : fullContext;
          throw new ExpectEmitMismatch(
            `${description}:\nFailed to find "${expectedPiece}"\nAfter "${context}"`,
            expectedPiece,
          );
        }
        errLast = subMatch.index + subMatch[0].length;
      }
      throw new ExpectEmitMismatch(
        `${description}: expected chunk failed but the failing token could not be located`,
        '<chunk>',
      );
    }

    for (const [id, idx] of newGroups.entries()) {
      extractedGroups.set(id, m[idx]);
    }
    lastIndex = m.index + m[0].length;
  }
}

/** @param {string | RegExp} piece */
function pieceText(piece) {
  return typeof piece === 'string' ? piece : piece.source;
}

// ---------------------------------------------------------------------------
// Expected-file preprocessing (expected_file_macros.ts, sourcemap_helpers.ts).
// ---------------------------------------------------------------------------

/**
 * `AttributeMarker` members (`compiler/src/core.ts`). Checked against the Angular source in the
 * compliance harness header; the numbers are Angular's, not Treaty's.
 */
const ATTRIBUTE_MARKER = {
  NamespaceURI: 0,
  Classes: 1,
  Styles: 2,
  Bindings: 3,
  Template: 4,
  ProjectAs: 5,
  I18n: 6,
};
/** `SelectorFlags` (`compiler/src/core.ts`). */
const SELECTOR_FLAGS = { NOT: 0b0001, ATTRIBUTE: 0b0010, ELEMENT: 0b0100, CLASS: 0b1000 };
/** `QueryFlags` (`core/src/render3/interfaces/query.ts`). */
const QUERY_FLAGS = { none: 0b0000, descendants: 0b0001, isStatic: 0b0010, emitDistinctChangesOnly: 0b0100 };

/**
 * Evaluate the expected-file macros in Angular's order (`expected_file_macros.ts`
 * `EXPECTED_FILE_MACROS`): the three i18n message macros first, then `__AttributeMarker`,
 * `__SelectorFlags` and `__QueryFlags`. The i18n macros are evaluated by the port of Angular's
 * `i18n_helpers.ts` below, so the golden they stand for is the exact text Angular's test expects.
 * @param {string} expectedContent
 * @returns {string}
 */
export function replaceMacros(expectedContent) {
  resetMessageIndex();
  for (const [regex, replacer] of I18N_MACROS) {
    expectedContent = expectedContent.replace(regex, replacer);
  }
  expectedContent = expectedContent.replace(
    /__AttributeMarker\.([^_]+)__/g,
    (_match, member) => {
      const value = ATTRIBUTE_MARKER[/** @type {keyof typeof ATTRIBUTE_MARKER} */ (member)];
      if (typeof value !== 'number') throw new Error('Unknown AttributeMarker: ' + member);
      return `${value}`;
    },
  );
  expectedContent = flagUnion(expectedContent, /__SelectorFlags\.([^_]+)__/, SELECTOR_FLAGS, 'SelectorFlag');
  expectedContent = flagUnion(expectedContent, /__QueryFlags\.([^_]+)__/, QUERY_FLAGS, 'QueryFlag');
  return expectedContent;
}

// ---------------------------------------------------------------------------
// i18n message macros (test_helpers/i18n_helpers.ts + expected_file_macros.ts).
// ---------------------------------------------------------------------------

/** Angular's `msgIndex`: the per-golden counter of `$I18N_<n>$` variables. Reset by `replaceMacros`. */
let msgIndex = 0;

/** @returns {void} */
function resetMessageIndex() {
  msgIndex = 0;
}

/**
 * `macroFn(fnName, ...args)`: the macro's call pattern, with optional whitespace around the name,
 * the parentheses, and every argument and comma.
 * @param {RegExp} fnName
 * @param {...RegExp} args
 * @returns {RegExp}
 */
function macroFn(fnName, ...args) {
  const ws = /[\s\r\n]*/.source;
  return new RegExp(
    ws + fnName.source + '\\(' + args.map((r) => `${ws}${r.source}${ws}`).join(',') + '\\)' + ws,
    'g',
  );
}

/** `stringParam()`: a single-quoted string. */
const STRING_PARAM = /'([^']*?[^\\])'/;
/** `arrayParam()`: a bracketed array. */
const ARRAY_PARAM = /(\[.*?\])/;
/**
 * `objectParam()`: an object literal with up to six levels of nesting (Angular's pattern, verbatim).
 */
const OBJECT_PARAM =
  /(\{[^{}]*(?:\{[^{}]*(?:\{[^{}]*(?:\{[^{}]*(?:\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}[^{}]*)*\}[^{}]*)*\}[^{}]*)*\}[^{}]*)*\})/;

/**
 * Evaluate an expected-file argument literal. Angular uses `eval`; the golden corpus is trusted
 * repository input, and an invalid literal is a malformed golden (a skip), not a mismatch.
 * @param {string} str
 * @returns {unknown}
 */
function evalLiteral(str) {
  try {
    return new Function(`return (${str});`)();
  } catch (err) {
    throw new ExpectEmitInvalid(`Invalid macro argument: ${str} (${/** @type {Error} */ (err).message})`);
  }
}

/**
 * `parsePlaceholders`: `[name, identifier, associatedId?]` tuples.
 * @param {string} str
 * @returns {Array<[string, string, string?]>}
 */
function parsePlaceholders(str) {
  const placeholders = evalLiteral(str);
  if (
    !Array.isArray(placeholders) ||
    !placeholders.every(
      (p) =>
        Array.isArray(p) &&
        p.length >= 2 &&
        typeof p[0] === 'string' &&
        typeof p[1] === 'string' &&
        (p.length === 2 || typeof p[2] === 'string'),
    )
  ) {
    throw new ExpectEmitInvalid(
      'Expected an array of Placeholder arrays (`[name: string, identifier: string, associatedId?: string]`) but got ' +
        str,
    );
  }
  return /** @type {any} */ (placeholders);
}

/**
 * `parseOptions`: an object whose only key is `original_code`, a string-valued map.
 * @param {string} str
 * @returns {{original_code?: Record<string, string>}}
 */
function parseOptions(str) {
  const inputObj = evalLiteral(str);
  if (typeof inputObj !== 'object' || inputObj === null) {
    throw new ExpectEmitInvalid(`Expected an object of properties but got:\n\n${str}.`);
  }
  const obj = /** @type {Record<string, unknown>} */ (inputObj);
  const unexpectedKeys = Object.keys(obj).filter((key) => key !== 'original_code');
  if (unexpectedKeys.length > 0) {
    throw new ExpectEmitInvalid(
      `Expected an i18n options object with \`original_code\`, but got ${unexpectedKeys.join(', ')}`,
    );
  }
  const original = obj['original_code'];
  if (typeof original !== 'undefined' && typeof original !== 'object') {
    throw new ExpectEmitInvalid(
      `Expected an i18n options object with \`original_code\`, as a nested object, but got ${JSON.stringify(obj, null, 4)}`,
    );
  }
  for (const [key, value] of Object.entries(original ?? {})) {
    if (typeof value !== 'string') {
      throw new ExpectEmitInvalid(
        `Expected an object whose values are strings, but property ${key} has type ${typeof value}, when parsing:\n\n${str}`,
      );
    }
  }
  return /** @type {any} */ (obj);
}

/**
 * `parseMetaProperties`: the `{desc, meaning, id}` bag, string-valued.
 * @param {string} str
 * @returns {Record<string, string>}
 */
function parseMetaProperties(str) {
  const obj = /** @type {Record<string, unknown>} */ (evalLiteral(str));
  if (typeof obj !== 'object' || obj === null) {
    throw new ExpectEmitInvalid(`Expected an object of properties but got:\n\n${str}.`);
  }
  for (const key in obj) {
    if (typeof obj[key] !== 'string') {
      throw new ExpectEmitInvalid(
        `Expected an object whose values are strings, but property ${key} has type ${typeof obj[key]}, when parsing:\n\n${str}`,
      );
    }
  }
  return /** @type {Record<string, string>} */ (obj);
}

/** `i18nMsg(message, placeholders, options, meta)` (i18n_helpers.ts). */
function i18nMsg(message, placeholders, options, meta) {
  const varName = `$I18N_${msgIndex++}$`;
  const closurePlaceholders = i18nPlaceholdersToString(placeholders);
  const closureOptions = i18nOptionsToString(options);
  const locMessageWithPlaceholders = i18nMsgInsertLocalizePlaceholders(message, placeholders);
  return `
    let ${varName};
    if (typeof ngI18nClosureMode !== "undefined" && ngI18nClosureMode) {
        ${i18nMsgClosureMeta(meta)}
        const $MSG_EXTERNAL_${msgIndex}$ = /* @ts-ignore */ goog.getMsg("${message}"${closurePlaceholders}${closureOptions});
        ${varName} = $MSG_EXTERNAL_${msgIndex}$;
    }
    else {
      /* @ts-ignore */
      ${varName} = $localize \`${i18nMsgLocalizeMeta(meta)}${locMessageWithPlaceholders}\`;
    }`;
}

/**
 * `i18nMsgWithPostprocess(...)`: `String.raw` in Angular, so the text is kept verbatim; the
 * postprocess variable is `$$<var>$$`, exactly as Angular's template spells it.
 */
function i18nMsgWithPostprocess(message, placeholders, options, meta, postprocessPlaceholders) {
  const varName = `$I18N_${msgIndex}$`;
  const ppPlaceholders = i18nPlaceholdersToString(postprocessPlaceholders);
  const inner = i18nMsg(message, placeholders, options, meta);
  return `
        ${inner}
        ${varName} = $r3$.ɵɵi18nPostprocess($${varName}$${ppPlaceholders});
      `;
}

/** `i18nIcuMsg(message, placeholders, options)`. */
function i18nIcuMsg(message, placeholders, options) {
  return i18nMsgWithPostprocess(message, [], options, {}, placeholders);
}

/** `i18nPlaceholdersToString`. */
function i18nPlaceholdersToString(placeholders) {
  if (placeholders.length === 0) return '';
  const result = placeholders.map(([name, identifier]) => `"${name}": ${quotedValue(identifier)}`);
  return `, { ${result.join(',')} }`;
}

/** `i18nOptionsToString`. */
function i18nOptionsToString({ original_code: originals = {} } = {}) {
  if (Object.keys(originals).length === 0) return '';
  const result = Object.entries(originals).map(([key, value]) => `"${key}": ${quotedValue(value)}`);
  return `, { original_code: {\n${result.join(',\n')}\n} }`;
}

/** `i18nMsgInsertLocalizePlaceholders`: `{$name}` becomes `${"identifier"}:UPPER_NAME@@assoc:`. */
function i18nMsgInsertLocalizePlaceholders(message, placeholders) {
  if (placeholders.length > 0) {
    message = message.replace(/{\$(.*?)}/g, (_, name) => {
      const placeholder = placeholders.find((p) => p[0] === name);
      if (!placeholder) throw new ExpectEmitInvalid(`No placeholder named ${name}`);
      // e.g. startDivTag -> START_DIV_TAG
      const key = name.replace(/[A-Z]/g, (ch) => '_' + ch).toUpperCase();
      const associated = placeholder.length === 3 ? `@@${placeholder[2]}` : '';
      return '$' + `{${quotedValue(placeholder[1])}}:${key}${associated}:`;
    });
  }
  return message;
}

/** `i18nMsgClosureMeta`: the `@desc` / `@meaning` JSDoc on the closure branch. */
function i18nMsgClosureMeta(meta) {
  if (!meta) return '';
  return `
    /**
     * ${meta.desc ? '@desc ' + meta.desc : '@suppress {msgDescriptions}'}
     ${meta.meaning ? '* @meaning ' + meta.meaning : ''}
     */
  `;
}

/** `i18nMsgLocalizeMeta`: the `:meaning|desc@@id:` prefix of the `$localize` template. */
function i18nMsgLocalizeMeta(meta) {
  if (!meta) return '';
  let localizeMeta = '';
  if (meta.meaning) localizeMeta += `${meta.meaning}|`;
  if (meta.desc) localizeMeta += meta.desc;
  if (meta.id) localizeMeta += `@@${meta.id}`;
  return localizeMeta !== '' ? `:${localizeMeta}:` : '';
}

/** `quotedValue`: a `$`-prefixed value is an ICU reference and stays unquoted. */
function quotedValue(value) {
  return value.startsWith('$') ? value : `"${value.replace(/"/g, '\\"')}"`;
}

/**
 * Angular's `EXPECTED_FILE_MACROS` i18n entries, in order. `macroFn` capture groups map one to one
 * onto the replacer's arguments (the wrapping whitespace has no capture group).
 */
const I18N_MACROS = [
  [
    macroFn(/__i18nMsg__/, STRING_PARAM, ARRAY_PARAM, OBJECT_PARAM, OBJECT_PARAM),
    (_match, message, placeholders, options, meta) =>
      i18nMsg(message, parsePlaceholders(placeholders), parseOptions(options), parseMetaProperties(meta)),
  ],
  [
    macroFn(/__i18nMsgWithPostprocess__/, STRING_PARAM, ARRAY_PARAM, OBJECT_PARAM, OBJECT_PARAM, ARRAY_PARAM),
    (_match, message, placeholders, options, meta, postProcessPlaceholders) =>
      i18nMsgWithPostprocess(
        message,
        parsePlaceholders(placeholders),
        parseOptions(options),
        parseMetaProperties(meta),
        parsePlaceholders(postProcessPlaceholders),
      ),
  ],
  [
    macroFn(/__i18nIcuMsg__/, STRING_PARAM, ARRAY_PARAM, OBJECT_PARAM),
    (_match, message, placeholders, options) =>
      i18nIcuMsg(message, parsePlaceholders(placeholders), parseOptions(options)),
  ],
];

/**
 * Port of `flagUnion`: each macro, and any `|`-joined run of them, becomes the bitwise OR of the
 * named flags. Angular's own pattern is kept verbatim, including its `s*\|s*` separator (a
 * literal `s`, not `\s`), so that a run is joined only as Angular joins it.
 * @param {string} text
 * @param {RegExp} pattern
 * @param {Record<string, number>} table
 * @param {string} kind
 */
function flagUnion(text, pattern, table, kind) {
  const union = new RegExp(pattern.source + '(?:s*\\|s*' + pattern.source + ')*', 'g');
  return text.replace(union, (match) =>
    String(
      match
        .split('|')
        .map((flag) => {
          const member = flag.trim().match(pattern);
          const value = member ? table[member[1]] : undefined;
          if (typeof value !== 'number') throw new Error(`Unknown ${kind}: ${flag}`);
          return value;
        })
        .reduce((acc, v) => acc | v, 0),
    ),
  );
}

/**
 * Angular's `extractMappings`: a `<generated> // SOURCE: "<url>" "<source>"` line asserts a source
 * map segment. The annotation is removed (the generated text is kept) and the count returned, so
 * the caller can report how many mapping assertions were not verified.
 * @param {string} expected
 * @returns {{expected: string, mappingAssertions: number}}
 */
export function stripSourceMappingAnnotations(expected) {
  let mappingAssertions = 0;
  const stripped = expected.replace(
    /^(.*?) \/\/ SOURCE: "([^"]*?)" "(.*?)"(?:\n|$)/gm,
    (_, generated) => {
      mappingAssertions++;
      return generated;
    },
  );
  return { expected: stripped, mappingAssertions };
}

// ---------------------------------------------------------------------------
// Extra checks (function_checks.ts, di_checks.ts). The i18n checks are not ported.
// ---------------------------------------------------------------------------

/**
 * `verifyUniqueFunctions(output, functionNamePattern?, expectedCount?)`.
 * @param {string} output
 * @param {string} [functionNamePattern]
 * @param {number} [expectedCount]
 */
export function verifyUniqueFunctions(output, functionNamePattern, expectedCount) {
  const pattern = functionNamePattern ? new RegExp(functionNamePattern) : null;
  const all = (output.match(/function ([^\s(]+)/g) || [])
    .map((match) => match.slice(9))
    .filter((name) => !pattern || pattern.test(name));
  const unique = new Set(all);
  const lengthMatches = all.length === unique.size;
  const countMatches = expectedCount == null ? all.length > 0 : all.length === expectedCount;
  return lengthMatches && countMatches && all.every((name) => unique.has(name));
}

/**
 * `verifyUniqueFactory(output, type)`: exactly one `function <type>_Factory`.
 * @param {string} output
 * @param {string} type
 */
export function verifyUniqueFactory(output, type) {
  const matches = output.match(new RegExp(`function ${type}_Factory`, 'g'));
  return matches !== null && matches.length === 1;
}

// ---------------------------------------------------------------------------
// i18n extra checks (test_helpers/i18n_checks.ts).
// ---------------------------------------------------------------------------

/** `EXTRACT_GENERATED_TRANSLATIONS_REGEXP`: `const <name> = goog.getMsg("<msg>", <placeholders>)`. */
const GENERATED_TRANSLATION = /const\s*(.*?)\s*=\s*goog\.getMsg\("(.*?)",?\s*(.*?)\)/g;

/**
 * Port of Angular's `extract`: every match of a fresh global copy of `regex`, transformed and
 * collected into a Set (the transform may throw).
 * @template T
 * @param {string} from
 * @param {RegExp} regex
 * @param {(match: RegExpExecArray, state: Set<T>) => T} transformFn
 * @returns {Set<T>}
 */
function extract(from, regex, transformFn) {
  const global = new RegExp(regex.source, 'g');
  const result = new Set();
  let item;
  while ((item = global.exec(from)) !== null) {
    result.add(transformFn(item, result));
  }
  return result;
}

/**
 * `verifyPlaceholdersIntegrity(output)`: in every `goog.getMsg` call, the `{$name}` placeholders of
 * the message are exactly the `"name": ...` keys of its placeholder object.
 * @param {string} output
 * @returns {boolean}
 */
export function verifyPlaceholdersIntegrity(output) {
  const translations = extract(output, GENERATED_TRANSLATION, (m) => [m[2], m[3]]);
  for (const [msg, args] of translations) {
    const bodyPhs = extract(msg, /{\$(.*?)}/g, (m) => m[1]);
    const argsPhs = extract(args, /\s+"(.+?)":\s/g, (m) => m[1]);
    const missing = [...bodyPhs].filter((x) => !argsPhs.has(x));
    if (bodyPhs.size !== argsPhs.size || missing.length > 0) return false;
  }
  return true;
}

/**
 * `verifyUniqueConsts(output)`: no two `goog.getMsg` constants share a name (Angular throws; false here).
 * @param {string} output
 * @returns {boolean}
 */
export function verifyUniqueConsts(output) {
  // Angular throws on a duplicate; a thrown check is a failed check, so it is reported as `false`.
  try {
    extract(output, GENERATED_TRANSLATION, (current, state) => {
      const key = current[1];
      if (state.has(key)) {
        throw new Error(`Duplicate const ${key} found in generated output!`);
      }
      return key;
    });
    return true;
  } catch {
    return false;
  }
}
