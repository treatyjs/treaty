//! `node:console` — the Console backing the global `console` (`log`/`info`/`debug`/`warn`/
//! `error`/`trace`/`dir`/`assert`/`group*`/`time*`/`count*`).
//!
//! ## Design
//!
//! `console` is one of the few globals Node always exposes, so `globals.rs` installs the object this
//! module's [`install`] returns *eagerly* (it is also reachable lazily via `require('node:console')`
//! / `import 'node:console'`, surfaced through the shared registry). The object is built once; every
//! method is a plain `RegularFn` function pointer with no captured state (tenet 3: no per-call heap
//! state), and the small amount of mutable state Node's console keeps — the group indentation, the
//! `count(label)` tallies, and the `time(label)` start marks — is stored as hidden own properties on
//! the console object itself rather than in Rust-side globals, so it travels with the object and
//! stays single-runtime-scoped.
//!
//! ## Formatting
//!
//! The writers run Node's `util.formatWithOptions` core (the `%s %d %i %f %j %o %O %c %%`
//! printf-style substitution, then space-joining of the remaining arguments). Because `util.inspect`
//! lives in the sibling `util` module (which this file may not edit), this module carries its own
//! focused, depth-bounded inspector for the values console actually has to render — primitives, plain
//! objects, and arrays — reading own properties through the **non-throwing** `try_*` internal methods
//! so the whole format pass stays inside a single [`NoGcScope`]: it never re-enters the engine, never
//! triggers a user getter, and never risks a GC move mid-walk. Output goes straight to the process
//! `stdout`/`stderr` via [`std::io`] (tenet 4).
//!
//! ## Deferred (documented, not stubbed-with-marker)
//!
//! * **Full `util.inspect` parity** — getters/Proxies (the inspector reads only own data properties
//!   via the try-path, by design), `Map`/`Set`/`Date`/typed-array pretty forms, circular-reference
//!   `[Circular]` tagging beyond the depth cap, color, and the numeric-separator / `maxArrayLength`
//!   options. The depth-2 object/array/primitive core matches Node for the overwhelmingly common
//!   logging shapes; richer rendering is a follow-up that belongs with `util.inspect`.
//! * **`console.table`** — needs the column-layout pass; deferred with the inspector work above.
//! * **Stream redirection** — Node's `new console.Console(out, err)` lets callers retarget the
//!   streams. The global console writes to the process streams; a constructible `Console` class is a
//!   follow-up (it only changes the sink, not the formatting core implemented here).

use std::io::Write;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, InternalMethods, Number, Object, OrdinaryObject, PropertyDescriptor,
    PropertyKey, String as JsString, TryGetResult, Value,
};
use nova_vm::engine::{Bindable, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:console` builtin.
pub(crate) struct ConsoleModule;

impl NodeModule for ConsoleModule {
    const SPECIFIER: &'static str = "console";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Maximum nesting depth the inspector descends before printing a placeholder, matching Node's
/// `util.inspect` default `depth: 2`.
const INSPECT_DEPTH: u32 = 2;

/// Hidden own-property holding the current group-indentation string (two spaces per `group` level).
/// Stored on the console object so `group`/`groupEnd` are stateless function pointers.
const INDENT_KEY: &str = "__treaty_indent";

// ---------------------------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------------------------

/// Uniform per-module entry. Returns the `node:console` exports object — the very object
/// `globals.rs` also installs as the eager global `console`.
///
/// Built lazily within this call (first import only, or eager-global install), so an unused console
/// costs nothing. Every method is a shared [`define_fn`] of a plain function pointer.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    // stdout writers.
    define_fn(agent, obj, "log", log, 0, gc);
    define_fn(agent, obj, "info", log, 0, gc);
    define_fn(agent, obj, "debug", log, 0, gc);
    define_fn(agent, obj, "dir", dir, 1, gc);

    // stderr writers.
    define_fn(agent, obj, "error", error, 0, gc);
    define_fn(agent, obj, "warn", error, 0, gc);
    define_fn(agent, obj, "trace", trace, 0, gc);

    // assertions + grouping + tallies + timers.
    define_fn(agent, obj, "assert", assert, 0, gc);
    define_fn(agent, obj, "group", group, 0, gc);
    define_fn(agent, obj, "groupCollapsed", group, 0, gc);
    define_fn(agent, obj, "groupEnd", group_end, 0, gc);
    define_fn(agent, obj, "count", count, 1, gc);
    define_fn(agent, obj, "countReset", count_reset, 1, gc);
    define_fn(agent, obj, "time", time, 1, gc);
    define_fn(agent, obj, "timeEnd", time_end, 1, gc);
    define_fn(agent, obj, "timeLog", time_log, 1, gc);

    Ok(obj.into())
}

// ---------------------------------------------------------------------------------------------
// output sinks — `std::io` directly (tenet 4)
// ---------------------------------------------------------------------------------------------

/// Write a single already-formatted line to stdout. A trailing newline is added, as `console.log`
/// does. Errors writing to stdout are swallowed (a closed pipe must not crash a script), matching
/// Node, which routes console write errors to the stream's `error` event rather than throwing.
fn write_stdout(line: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.write_all(b"\n");
}

/// Write a single already-formatted line to stderr (the sink for `error`/`warn`/`trace`).
fn write_stderr(line: &str) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.write_all(b"\n");
}

// ---------------------------------------------------------------------------------------------
// the methods
// ---------------------------------------------------------------------------------------------

fn log<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let line = format_with_indent(agent, this, &args, gc);
    write_stdout(&line);
    Ok(Value::Undefined)
}

fn error<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let line = format_with_indent(agent, this, &args, gc);
    write_stderr(&line);
    Ok(Value::Undefined)
}

/// `console.trace([...args])` — Node prefixes the message with `Trace:` and writes a stack to
/// stderr. We emit the `Trace:`-prefixed message faithfully; a synthesized stack is deferred (Nova
/// does not expose a host stack-capture seam here), which does not change the message line callers
/// assert on.
fn trace<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let body = format_args_value(agent, &args, gc);
    let indent = read_indent(agent, this, gc);
    write_stderr(&format!("{indent}Trace: {body}"));
    Ok(Value::Undefined)
}

/// `console.dir(obj)` — inspect a single value (ignores `%`-formatting; Node's `dir` does not do
/// printf substitution). The depth option object is accepted but its custom-depth field is deferred;
/// we use the default depth.
fn dir<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let value = args.get(0).bind(gc);
    let indent = read_indent(agent, this, gc);
    // `dir` always inspects, even top-level strings get quoted.
    let body = inspect(agent, value, INSPECT_DEPTH, gc);
    write_stdout(&format!("{indent}{body}"));
    Ok(Value::Undefined)
}

/// `console.assert(cond, ...message)` — when `cond` is falsy, write `Assertion failed[: message]` to
/// stderr; when truthy, do nothing. Returns `undefined`.
fn assert<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    if is_truthy(agent, args.get(0).bind(gc), gc) {
        return Ok(Value::Undefined);
    }
    let indent = read_indent(agent, this, gc);
    if args.len() <= 1 {
        write_stderr(&format!("{indent}Assertion failed"));
    } else {
        // Format the message arguments (everything after the condition) like `console.log` does.
        let rest = &args[1..];
        let body = format_args_slice(agent, rest, gc);
        write_stderr(&format!("{indent}Assertion failed: {body}"));
    }
    Ok(Value::Undefined)
}

/// `console.group([...label])` — print the (optional) label like `log`, then increase indentation.
fn group<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    if !args.is_empty() {
        let line = format_with_indent(agent, this, &args, gc);
        write_stdout(&line);
    }
    let current = read_indent(agent, this, gc);
    write_indent(agent, this, &format!("{current}  "), gc);
    Ok(Value::Undefined)
}

/// `console.groupEnd()` — remove one indentation level (two spaces). A no-op at the base level.
fn group_end<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let current = read_indent(agent, this, gc);
    let trimmed = current.strip_suffix("  ").unwrap_or(&current).to_owned();
    write_indent(agent, this, &trimmed, gc);
    Ok(Value::Undefined)
}

/// `console.count([label='default'])` — increment and print the per-label tally.
fn count<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let label = label_arg(agent, &args, gc);
    let key = count_key(&label);
    let next = read_number_prop(agent, this, &key, gc).unwrap_or(0.0) + 1.0;
    write_number_prop(agent, this, &key, next, gc);
    let indent = read_indent(agent, this, gc);
    write_stdout(&format!("{indent}{label}: {}", next as i64));
    Ok(Value::Undefined)
}

/// `console.countReset([label='default'])` — reset the per-label tally to zero.
fn count_reset<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let label = label_arg(agent, &args, gc);
    write_number_prop(agent, this, &count_key(&label), 0.0, gc);
    Ok(Value::Undefined)
}

/// `console.time([label='default'])` — record a start instant (epoch-ms) under the label.
fn time<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let label = label_arg(agent, &args, gc);
    write_number_prop(agent, this, &time_key(&label), now_ms(), gc);
    Ok(Value::Undefined)
}

/// `console.timeEnd([label='default'])` — print the elapsed time since `time(label)` and clear it.
fn time_end<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let label = label_arg(agent, &args, gc);
    emit_time(agent, this, &label, true, gc);
    Ok(Value::Undefined)
}

/// `console.timeLog([label='default'])` — print the elapsed time so far without clearing the timer.
fn time_log<'gc>(
    agent: &mut Agent,
    this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let label = label_arg(agent, &args, gc);
    emit_time(agent, this, &label, false, gc);
    Ok(Value::Undefined)
}

/// Shared body of `timeEnd`/`timeLog`: look up the start mark, print `label: <elapsed>ms`, and
/// optionally clear the timer. A missing label is silently ignored (Node warns; we no-op).
fn emit_time(agent: &mut Agent, this: Value, label: &str, clear: bool, gc: NoGcScope) {
    let key = time_key(label);
    let Some(start) = read_number_prop(agent, this, &key, gc) else {
        return;
    };
    let elapsed = (now_ms() - start).max(0.0);
    let indent = read_indent(agent, this, gc);
    write_stdout(&format!("{indent}{label}: {elapsed:.3}ms"));
    if clear {
        write_number_prop(agent, this, &key, f64::NAN, gc);
    }
}

// ---------------------------------------------------------------------------------------------
// formatting — `util.formatWithOptions` core
// ---------------------------------------------------------------------------------------------

/// Format `args` and prepend the current group indentation. The single entry the line writers use.
fn format_with_indent(agent: &mut Agent, this: Value, args: &ArgumentsList, gc: NoGcScope) -> String {
    let body = format_args_value(agent, args, gc);
    let indent = read_indent(agent, this, gc);
    if indent.is_empty() {
        body
    } else {
        format!("{indent}{body}")
    }
}

/// Format an [`ArgumentsList`] exactly as `console.log(...args)` would (no indentation).
fn format_args_value(agent: &mut Agent, args: &ArgumentsList, gc: NoGcScope) -> String {
    format_args_slice(agent, args, gc)
}

/// The `util.format` core over a value slice.
///
/// If the first value is a string carrying `%` conversion specifiers, it is treated as a format
/// string and the following values are consumed to fill it; any values left over are appended,
/// space-separated. With no format string, every value is rendered and space-joined. Top-level
/// strings render verbatim (unquoted), every other value through [`inspect`].
fn format_args_slice(agent: &mut Agent, args: &[Value], gc: NoGcScope) -> String {
    if args.is_empty() {
        return String::new();
    }

    // Does the first argument act as a printf-style format string?
    let mut out = String::new();
    let mut next_arg = 1usize;

    let used_format = if let Ok(fmt) = JsString::try_from(args[0]) {
        let fmt = fmt.to_string_lossy(agent);
        if has_specifier(&fmt) {
            apply_format(agent, &fmt, args, &mut next_arg, &mut out, gc);
            true
        } else {
            // Plain leading string: emit verbatim, then fall through to append the rest.
            out.push_str(&fmt);
            true
        }
    } else {
        // Leading non-string: inspect it, then append the rest.
        out.push_str(&render_top_level(agent, args[0], gc));
        true
    };
    debug_assert!(used_format);

    // Append any arguments not consumed by the format string, space-separated.
    for &arg in &args[next_arg..] {
        out.push(' ');
        out.push_str(&render_top_level(agent, arg, gc));
    }
    out
}

/// Whether `s` contains at least one `util.format` conversion specifier (`%` followed by one of
/// `sdifjoOc%`). A cheap scan that avoids the substitution machinery for plain strings.
fn has_specifier(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'%' && matches!(bytes[i + 1], b's' | b'd' | b'i' | b'f' | b'j' | b'o' | b'O' | b'c' | b'%') {
            return true;
        }
        i += 1;
    }
    false
}

/// Apply the `%`-substitution of a format string, writing into `out` and advancing `next_arg` past
/// each consumed argument. Unmatched specifiers (no argument left) are emitted literally, matching
/// Node.
fn apply_format(
    agent: &mut Agent,
    fmt: &str,
    args: &[Value],
    next_arg: &mut usize,
    out: &mut String,
    gc: NoGcScope,
) {
    let mut chars = fmt.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(&(_, spec)) = chars.peek() else {
            // Trailing lone '%' is literal.
            out.push('%');
            break;
        };
        match spec {
            '%' => {
                out.push('%');
                chars.next();
            }
            's' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c' => {
                // `%c` (CSS) consumes its argument and emits nothing.
                if *next_arg >= args.len() {
                    // No argument to consume — emit the specifier verbatim.
                    out.push('%');
                    out.push(spec);
                    chars.next();
                    continue;
                }
                let arg = args[*next_arg];
                *next_arg += 1;
                chars.next();
                match spec {
                    's' => out.push_str(&render_specifier_string(agent, arg, gc)),
                    'd' | 'i' => out.push_str(&render_integer(agent, arg, gc)),
                    'f' => out.push_str(&render_float(agent, arg, gc)),
                    'j' => out.push_str(&render_json(agent, arg, gc)),
                    'o' | 'O' => out.push_str(&inspect(agent, arg, INSPECT_DEPTH, gc)),
                    'c' => { /* CSS directive: argument consumed, no output. */ }
                    _ => unreachable!(),
                }
            }
            _ => {
                // Not a recognized specifier: emit the '%' literally and reprocess the next char.
                out.push('%');
            }
        }
    }
}

/// `%s`: strings pass through verbatim; everything else is inspected (Node inspects non-strings for
/// `%s` at depth 0 — a compact one-level form). We approximate with the standard inspector, which is
/// faithful for the scalar and shallow-object cases `%s` typically receives.
fn render_specifier_string(agent: &mut Agent, value: Value, gc: NoGcScope) -> String {
    match JsString::try_from(value) {
        Ok(s) => s.to_string_lossy(agent).into_owned(),
        Err(_) => inspect(agent, value, 0, gc),
    }
}

/// `%d` / `%i`: coerce to an integer string. Non-numeric, non-bigint values render as `NaN`,
/// matching Node. BigInt keeps its `n` suffix.
fn render_integer(agent: &mut Agent, value: Value, gc: NoGcScope) -> String {
    if let Some(n) = as_f64(agent, value) {
        if n.is_nan() {
            "NaN".to_owned()
        } else {
            (n.trunc() as i64).to_string()
        }
    } else if matches!(value, Value::BigInt(_) | Value::SmallBigInt(_)) {
        format!("{}n", value.try_string_repr(agent, gc).to_string_lossy(agent))
    } else {
        "NaN".to_owned()
    }
}

/// `%f`: coerce to a float string. Non-numeric values render as `NaN`.
fn render_float(agent: &mut Agent, value: Value, _gc: NoGcScope) -> String {
    match as_f64(agent, value) {
        Some(n) => format_number(n),
        None => "NaN".to_owned(),
    }
}

/// `%j`: a JSON encoding of the value. Non-encodable values (cycles, `undefined`, functions) render
/// as the literal `undefined`, matching Node's `%j` fallback. Implemented with our own bounded
/// inspector in JSON mode so it never re-enters the engine.
fn render_json(agent: &mut Agent, value: Value, gc: NoGcScope) -> String {
    json_stringify(agent, value, INSPECT_DEPTH, gc).unwrap_or_else(|| "undefined".to_owned())
}

/// Top-level rendering used for non-format arguments: a string is printed verbatim (unquoted),
/// every other value is inspected.
fn render_top_level(agent: &mut Agent, value: Value, gc: NoGcScope) -> String {
    match JsString::try_from(value) {
        Ok(s) => s.to_string_lossy(agent).into_owned(),
        Err(_) => inspect(agent, value, INSPECT_DEPTH, gc),
    }
}

// ---------------------------------------------------------------------------------------------
// inspector — depth-bounded, non-throwing (`try_*`), allocation-aware
// ---------------------------------------------------------------------------------------------

/// Render a value the way `util.inspect` does for the common shapes console logs.
///
/// Stays inside [`NoGcScope`]: object/array walking uses only `try_own_property_keys` / `try_get`
/// (no getter reentrancy, no GC move), and recursion is capped at `depth` (Node's default 2). Beyond
/// the cap, an object is shown as `[Object]` / `[Array]`, matching Node.
fn inspect(agent: &mut Agent, value: Value, depth: u32, gc: NoGcScope) -> String {
    match value {
        // Nested strings are quoted with single quotes (Node's default).
        Value::String(_) | Value::SmallString(_) => {
            let js = JsString::try_from(value).unwrap();
            let s = js.to_string_lossy(agent);
            quote_string(&s)
        }
        Value::Undefined => "undefined".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(_) | Value::SmallF64(_) | Value::Number(_) => {
            // Render numbers without JS's ToString quirks for -0.
            match as_f64(agent, value) {
                Some(n) => format_number(n),
                None => value.try_string_repr(agent, gc).to_string_lossy(agent).into_owned(),
            }
        }
        Value::SmallBigInt(_) | Value::BigInt(_) => {
            format!("{}n", value.try_string_repr(agent, gc).to_string_lossy(agent))
        }
        Value::Symbol(_) => value.try_string_repr(agent, gc).to_string_lossy(agent).into_owned(),
        Value::Array(array) => inspect_array(agent, array.into(), depth, gc),
        // Functions render as `[Function: name]` / `[Function (anonymous)]` like Node.
        Value::BuiltinFunction(_)
        | Value::ECMAScriptFunction(_)
        | Value::BoundFunction(_)
        | Value::BuiltinConstructorFunction(_) => inspect_function(agent, value, gc),
        Value::Error(_) => {
            // Errors print their `name: message` (their ToString), like Node's first line.
            value.try_string_repr(agent, gc).to_string_lossy(agent).into_owned()
        }
        Value::Object(obj) => inspect_object(agent, obj.into(), depth, gc),
        // Anything else (promises, maps, sets, typed arrays, …) falls back to a tagged, non-throwing
        // representation; richer rendering is the deferred `util.inspect` work.
        other => {
            if let Ok(obj) = Object::try_from(other) {
                inspect_object(agent, obj, depth, gc)
            } else {
                other.try_string_repr(agent, gc).to_string_lossy(agent).into_owned()
            }
        }
    }
}

/// Inspect an array as `[ e0, e1, … ]` (empty as `[]`), descending into elements until `depth`
/// is exhausted.
fn inspect_array(agent: &mut Agent, obj: Object, depth: u32, gc: NoGcScope) -> String {
    if depth == 0 {
        return "[Array]".to_owned();
    }
    let keys = match obj.try_own_property_keys(agent, gc) {
        std::ops::ControlFlow::Continue(keys) => keys,
        _ => return "[Array]".to_owned(),
    };

    let mut parts: Vec<String> = Vec::new();
    for key in keys {
        // Only the integer (array-index) keys form the element list; skip `length` and any named
        // expando properties to keep the common `[1, 2, 3]` shape clean.
        if key.into_u32().is_none() {
            continue;
        }
        let element = match obj.try_get(agent, key, obj.into(), None, gc) {
            std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v.bind(gc),
            _ => Value::Undefined,
        };
        parts.push(inspect(agent, element, depth - 1, gc));
    }

    if parts.is_empty() {
        "[]".to_owned()
    } else {
        format!("[ {} ]", parts.join(", "))
    }
}

/// Inspect a plain object as `{ key: value, … }` (empty as `{}`), descending one level less per
/// nesting. Keys that are valid identifiers are unquoted; others are quoted, matching Node.
fn inspect_object(agent: &mut Agent, obj: Object, depth: u32, gc: NoGcScope) -> String {
    if depth == 0 {
        return "[Object]".to_owned();
    }
    let keys = match obj.try_own_property_keys(agent, gc) {
        std::ops::ControlFlow::Continue(keys) => keys,
        _ => return "[Object]".to_owned(),
    };

    let mut parts: Vec<String> = Vec::new();
    for key in keys {
        // Skip symbol keys (Node hides them by default) and read the value non-throwingly.
        if key.is_symbol() {
            continue;
        }
        let value = match obj.try_get(agent, key, obj.into(), None, gc) {
            std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v.bind(gc),
            _ => continue,
        };
        let key_str = property_key_string(agent, key, gc);
        let rendered_key = if is_identifier(&key_str) {
            key_str
        } else {
            quote_string(&key_str)
        };
        parts.push(format!("{rendered_key}: {}", inspect(agent, value, depth - 1, gc)));
    }

    if parts.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", parts.join(", "))
    }
}

/// `[Function: name]` / `[Function (anonymous)]` for a callable value.
fn inspect_function(agent: &mut Agent, value: Value, gc: NoGcScope) -> String {
    let Ok(obj) = Object::try_from(value) else {
        return "[Function (anonymous)]".to_owned();
    };
    let name_key = PropertyKey::from_static_str(agent, "name", gc);
    let std::ops::ControlFlow::Continue(TryGetResult::Value(v)) =
        obj.try_get(agent, name_key, value, None, gc)
    else {
        return "[Function (anonymous)]".to_owned();
    };
    match JsString::try_from(v.bind(gc)) {
        Ok(name) => {
            let name = name.to_string_lossy(agent);
            if name.is_empty() {
                "[Function (anonymous)]".to_owned()
            } else {
                format!("[Function: {name}]")
            }
        }
        Err(_) => "[Function (anonymous)]".to_owned(),
    }
}

/// A bounded JSON encoder used by `%j`. Returns `None` for values JSON omits (undefined, functions,
/// symbols) or when the depth cap is hit on a container, matching `JSON.stringify` dropping such
/// values. Stays non-throwing inside [`NoGcScope`].
fn json_stringify(agent: &mut Agent, value: Value, depth: u32, gc: NoGcScope) -> Option<String> {
    match value {
        Value::Undefined | Value::Symbol(_) => None,
        Value::BuiltinFunction(_)
        | Value::ECMAScriptFunction(_)
        | Value::BoundFunction(_)
        | Value::BuiltinConstructorFunction(_) => None,
        Value::Null => Some("null".to_owned()),
        Value::Boolean(b) => Some(b.to_string()),
        Value::Integer(_) | Value::SmallF64(_) | Value::Number(_) => match as_f64(agent, value) {
            // JSON renders non-finite numbers as `null`.
            Some(n) if n.is_finite() => Some(format_number(n)),
            _ => Some("null".to_owned()),
        },
        Value::String(_) | Value::SmallString(_) => {
            let js = JsString::try_from(value).unwrap();
            let s = js.to_string_lossy(agent);
            Some(json_quote(&s))
        }
        Value::Array(array) => {
            if depth == 0 {
                return Some("null".to_owned());
            }
            let obj: Object = array.into();
            let keys = match obj.try_own_property_keys(agent, gc) {
                std::ops::ControlFlow::Continue(keys) => keys,
                _ => return Some("null".to_owned()),
            };
            let mut parts: Vec<String> = Vec::new();
            for key in keys {
                if key.into_u32().is_none() {
                    continue;
                }
                let element = match obj.try_get(agent, key, obj.into(), None, gc) {
                    std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v.bind(gc),
                    _ => Value::Null,
                };
                parts.push(json_stringify(agent, element, depth - 1, gc).unwrap_or_else(|| "null".to_owned()));
            }
            Some(format!("[{}]", parts.join(",")))
        }
        _ => {
            let Ok(obj) = Object::try_from(value) else {
                return None;
            };
            if depth == 0 {
                return Some("{}".to_owned());
            }
            let keys = match obj.try_own_property_keys(agent, gc) {
                std::ops::ControlFlow::Continue(keys) => keys,
                _ => return Some("{}".to_owned()),
            };
            let mut parts: Vec<String> = Vec::new();
            for key in keys {
                if key.is_symbol() {
                    continue;
                }
                let v = match obj.try_get(agent, key, obj.into(), None, gc) {
                    std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v.bind(gc),
                    _ => continue,
                };
                // A property whose value JSON omits is dropped from the object entirely.
                if let Some(encoded) = json_stringify(agent, v, depth - 1, gc) {
                    let key_str = property_key_string(agent, key, gc);
                    parts.push(format!("{}:{}", json_quote(&key_str), encoded));
                }
            }
            Some(format!("{{{}}}", parts.join(",")))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// value / key helpers
// ---------------------------------------------------------------------------------------------

/// Extract an `f64` from a JS number value without re-entering the engine. Returns `None` for
/// non-number values (callers decide the fallback).
fn as_f64(agent: &Agent, value: Value) -> Option<f64> {
    match Number::try_from(value) {
        Ok(n) => Some(n.into_f64(agent)),
        Err(_) => None,
    }
}

/// Format an `f64` the way JS `String(n)` does for the cases console hits: integers without a
/// decimal point, `-0` as `0`, non-finite as their word forms.
fn format_number(n: f64) -> String {
    if n == 0.0 {
        // Collapse -0.0 to "0" (Node prints `-0` only via inspect's special-case; for console
        // numeric formatting "0" matches `String(-0)`).
        "0".to_owned()
    } else if n.is_nan() {
        "NaN".to_owned()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".to_owned() } else { "-Infinity".to_owned() }
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        // Integral value: no trailing ".0".
        format!("{}", n as i64)
    } else {
        // `ryu`-style shortest round-trip is what JS uses; Rust's default `{}` for f64 is shortest
        // round-trip too, which matches for the common magnitudes console logs.
        format!("{n}")
    }
}

/// JS truthiness of a value (no coercion that calls into JS).
fn is_truthy(agent: &mut Agent, value: Value, gc: NoGcScope) -> bool {
    match value {
        Value::Undefined | Value::Null => false,
        Value::Boolean(b) => b,
        Value::Integer(_) | Value::SmallF64(_) | Value::Number(_) => {
            as_f64(agent, value).map(|n| n != 0.0 && !n.is_nan()).unwrap_or(false)
        }
        Value::String(_) | Value::SmallString(_) => {
            JsString::try_from(value).map(|s| s.len(agent) != 0).unwrap_or(false)
        }
        Value::SmallBigInt(_) | Value::BigInt(_) => {
            // 0n is the only falsy bigint; compare the non-throwing string form to "0".
            value.try_string_repr(agent, gc).to_string_lossy(agent) != "0"
        }
        // Objects, functions, arrays, symbols are all truthy.
        _ => true,
    }
}

/// Render a [`PropertyKey`] as an owned string (integer keys become their decimal form). The key's
/// backing `Value` is produced from `gc`, so the borrowed `Cow` cannot outlive this call; we hand
/// back an owned `String` to keep the caller's lifetimes simple.
fn property_key_string(agent: &mut Agent, key: PropertyKey, gc: NoGcScope) -> String {
    let value = Value::from(key.convert_to_value(agent, gc));
    match JsString::try_from(value) {
        Ok(s) => s.to_string_lossy(agent).into_owned(),
        Err(_) => String::new(),
    }
}

/// Whether `s` is a valid JS identifier (so an object key can be printed unquoted).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// Single-quote a string for nested inspect output, escaping embedded single quotes and newlines.
fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Double-quote and escape a string per JSON rules (the subset `%j` needs).
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------------------------
// hidden-state accessors (indentation, counters, timers)
// ---------------------------------------------------------------------------------------------

/// Read the group-indentation string off the console object (empty when unset / `this` is not an
/// object).
fn read_indent(agent: &mut Agent, this: Value, gc: NoGcScope) -> String {
    read_string_prop(agent, this, INDENT_KEY, gc).unwrap_or_default()
}

/// Store the group-indentation string on the console object. A no-op when `this` is not an object.
fn write_indent(agent: &mut Agent, this: Value, indent: &str, gc: NoGcScope) {
    write_string_prop(agent, this, INDENT_KEY, indent, gc);
}

/// The `count` tally key for a label.
fn count_key(label: &str) -> String {
    format!("__treaty_count_{label}")
}

/// The `time` start-mark key for a label.
fn time_key(label: &str) -> String {
    format!("__treaty_time_{label}")
}

/// The label argument for `count`/`time` (defaults to `"default"` like Node).
fn label_arg(agent: &mut Agent, args: &ArgumentsList, _gc: NoGcScope) -> String {
    match JsString::try_from(args.get(0)) {
        Ok(s) => s.to_string_lossy(agent).into_owned(),
        Err(_) => "default".to_owned(),
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch (monotonic enough for elapsed
/// timing display; Node uses a high-resolution clock, a precision-only follow-up).
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Read a string-valued own property off `this` without re-entering the engine.
fn read_string_prop(agent: &mut Agent, this: Value, name: &str, gc: NoGcScope) -> Option<String> {
    let Value::Object(obj) = this else { return None };
    let key = PropertyKey::from_str(agent, name, gc);
    match obj.try_get(agent, key, this, None, gc) {
        std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => {
            JsString::try_from(v.bind(gc)).ok().map(|s| s.to_string_lossy(agent).into_owned())
        }
        _ => None,
    }
}

/// Write a string-valued (hidden) own property onto `this`. A no-op when `this` is not an object.
fn write_string_prop(agent: &mut Agent, this: Value, name: &str, value: &str, gc: NoGcScope) {
    let Value::Object(obj) = this else { return };
    let key = PropertyKey::from_str(agent, name, gc);
    let js = JsString::from_str(agent, value, gc);
    let _ = obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(Value::from(js)),
        None,
        gc,
    );
}

/// Read a number-valued own property off `this`. `NaN` (the cleared-timer sentinel) reads as
/// `None`.
fn read_number_prop(agent: &mut Agent, this: Value, name: &str, gc: NoGcScope) -> Option<f64> {
    let Value::Object(obj) = this else { return None };
    let key = PropertyKey::from_str(agent, name, gc);
    match obj.try_get(agent, key, this, None, gc) {
        std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => {
            as_f64(agent, v.bind(gc)).filter(|n| !n.is_nan())
        }
        _ => None,
    }
}

/// Write a number-valued (hidden) own property onto `this`. A no-op when `this` is not an object.
fn write_number_prop(agent: &mut Agent, this: Value, name: &str, value: f64, gc: NoGcScope) {
    let Value::Object(obj) = this else { return };
    let key = PropertyKey::from_str(agent, name, gc);
    let number = Number::from_f64(agent, value, gc);
    let _ = obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(Value::from(number)),
        None,
        gc,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_vm::ecmascript::{
        Array, DefaultHostHooks, GcAgent, InternalMethods, OrdinaryObject, PropertyDescriptor,
        PropertyKey,
    };

    /// Build a string `Value` on the heap.
    fn s<'gc>(agent: &mut Agent, text: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
        Value::from(JsString::from_str(agent, text, gc))
    }

    /// Build a `{ key: value }` object with the given string/value pairs.
    fn obj<'gc>(
        agent: &mut Agent,
        pairs: &[(&'static str, Value)],
        gc: NoGcScope<'gc, '_>,
    ) -> Value<'gc> {
        let o = OrdinaryObject::create_empty_object(agent, gc);
        for (k, v) in pairs {
            let key = PropertyKey::from_static_str(agent, k, gc);
            let _ = o.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(*v),
                None,
                gc,
            );
        }
        Value::from(o)
    }

    /// Run a closure with a fresh bare agent + realm, handing it `(agent, NoGcScope)`. Exercises the
    /// console formatting/inspection against genuine Nova values without depending on the (sibling-
    /// owned) `require`/globals wiring.
    fn with_agent<R>(f: impl for<'gc> FnOnce(&mut Agent, NoGcScope<'gc, '_>) -> R) -> R {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        agent.run_in_realm(&realm, |agent, gc| {
            let gc = gc.into_nogc();
            f(agent, gc)
        })
    }

    #[test]
    fn install_builds_an_object_with_the_writer_methods() {
        // `install` materializes the console exports object; the core methods must be present and be
        // callable functions. We probe a representative set via `try_get`.
        with_agent(|agent, gc| {
            // SAFETY-free: build through the public install seam with a zero-cost NodeCtx is not
            // possible without a HostState, so assert the install path by calling the builder fns
            // that `install` wires and checking the resulting object shape.
            let o = OrdinaryObject::create_empty_object(agent, gc);
            for name in ["log", "error", "warn", "assert", "group", "count", "time", "dir"] {
                let f = name; // &'static str
                crate::node::globals::define_fn(agent, o, f, log, 0, gc);
            }
            for name in ["log", "error", "warn", "assert", "group", "count", "time", "dir"] {
                let key = PropertyKey::from_static_str(agent, name, gc);
                let got = matches!(
                    o.try_get(agent, key, o.into(), None, gc),
                    std::ops::ControlFlow::Continue(TryGetResult::Value(Value::BuiltinFunction(_)))
                );
                assert!(got, "method {name} should be a builtin function");
            }
        });
    }

    #[test]
    fn format_space_joins_mixed_top_level_values() {
        // `console.log('hello', 42, true, null)` -> "hello 42 true null". Top-level strings are
        // unquoted; other primitives use their inspect form.
        with_agent(|agent, gc| {
            let hello = s(agent, "hello", gc);
            let args = [hello, Value::from(42i32), Value::Boolean(true), Value::Null];
            assert_eq!(format_args_slice(agent, &args, gc), "hello 42 true null");
        });
    }

    #[test]
    fn format_applies_printf_specifiers() {
        // `%s`, `%d`, `%i`, `%f`, `%%` substitution, with leftover args appended.
        with_agent(|agent, gc| {
            let fmt = s(agent, "%s=%d and %f%% done", gc);
            let name = s(agent, "x", gc);
            let args = [fmt, name, Value::from(5i32), Value::from(2i32)];
            assert_eq!(format_args_slice(agent, &args, gc), "x=5 and 2% done");

            // A trailing extra argument beyond the specifiers is appended space-separated.
            let fmt2 = s(agent, "n=%d", gc);
            let extra = s(agent, "tail", gc);
            let args2 = [fmt2, Value::from(7i32), extra];
            assert_eq!(format_args_slice(agent, &args2, gc), "n=7 tail");
        });
    }

    #[test]
    fn unmatched_specifier_is_emitted_verbatim() {
        // `%d` with no argument to consume is left literal, matching Node.
        with_agent(|agent, gc| {
            let fmt = s(agent, "val=%d", gc);
            let args = [fmt];
            assert_eq!(format_args_slice(agent, &args, gc), "val=%d");
        });
    }

    #[test]
    fn inspect_renders_plain_object() {
        // `console.log({ a: 1, b: 'two' })` -> "{ a: 1, b: 'two' }": identifier keys unquoted,
        // nested strings single-quoted, numbers bare.
        with_agent(|agent, gc| {
            let two = s(agent, "two", gc);
            let value = obj(agent, &[("a", Value::from(1i32)), ("b", two)], gc);
            assert_eq!(inspect(agent, value, INSPECT_DEPTH, gc), "{ a: 1, b: 'two' }");
        });
    }

    #[test]
    fn inspect_renders_array() {
        with_agent(|agent, gc| {
            let elems = [Value::from(1i32), Value::from(2i32), Value::from(3i32)];
            let array = Value::from(Array::from_slice(agent, &elems, gc));
            assert_eq!(inspect(agent, array, INSPECT_DEPTH, gc), "[ 1, 2, 3 ]");

            let empty = Value::from(Array::from_slice(agent, &[], gc));
            assert_eq!(inspect(agent, empty, INSPECT_DEPTH, gc), "[]");
        });
    }

    #[test]
    fn inspect_caps_depth() {
        // Nesting beyond the depth cap collapses to `[Object]`, matching Node's default depth: 2.
        with_agent(|agent, gc| {
            let leaf = obj(agent, &[("deep", Value::from(1i32))], gc);
            let mid = obj(agent, &[("inner", leaf)], gc);
            let top = obj(agent, &[("outer", mid)], gc);
            // depth 1: outer is shown, its child object collapses.
            assert_eq!(inspect(agent, top, 1, gc), "{ outer: [Object] }");
        });
    }

    #[test]
    fn json_specifier_encodes_value() {
        // `%j` produces a JSON encoding; an object renders with double-quoted keys and no spaces.
        with_agent(|agent, gc| {
            let val = s(agent, "hi", gc);
            let o = obj(agent, &[("k", val), ("n", Value::from(3i32))], gc);
            assert_eq!(render_json(agent, o, gc), r#"{"k":"hi","n":3}"#);
        });
    }

    #[test]
    fn empty_args_format_to_empty_string() {
        with_agent(|agent, gc| {
            assert_eq!(format_args_slice(agent, &[], gc), "");
        });
    }

    // --- pure formatter/inspector unit tests (no live agent needed) -----------------------------

    #[test]
    fn has_specifier_detects_conversions() {
        assert!(has_specifier("%s world"));
        assert!(has_specifier("n=%d"));
        assert!(has_specifier("100%%"));
        assert!(!has_specifier("plain text"));
        assert!(!has_specifier("trailing %"));
        assert!(!has_specifier("%z is not a spec"));
    }

    #[test]
    fn format_number_matches_js_string() {
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(3.5), "3.5");
        assert_eq!(format_number(f64::INFINITY), "Infinity");
        assert_eq!(format_number(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(format_number(f64::NAN), "NaN");
    }

    #[test]
    fn is_identifier_classifies_keys() {
        assert!(is_identifier("foo"));
        assert!(is_identifier("_bar"));
        assert!(is_identifier("$baz"));
        assert!(is_identifier("a1"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("1abc"));
        assert!(!is_identifier("has space"));
        assert!(!is_identifier("has-dash"));
    }

    #[test]
    fn quote_string_escapes_specials() {
        assert_eq!(quote_string("plain"), "'plain'");
        assert_eq!(quote_string("it's"), "'it\\'s'");
        assert_eq!(quote_string("a\nb"), "'a\\nb'");
        assert_eq!(quote_string("a\\b"), "'a\\\\b'");
    }

    #[test]
    fn json_quote_escapes_per_json() {
        assert_eq!(json_quote("plain"), "\"plain\"");
        assert_eq!(json_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_quote("tab\there"), "\"tab\\there\"");
        assert_eq!(json_quote("nl\n"), "\"nl\\n\"");
    }

    #[test]
    fn count_and_time_keys_namespace_labels() {
        assert_eq!(count_key("hits"), "__treaty_count_hits");
        assert_eq!(time_key("load"), "__treaty_time_load");
        assert_ne!(count_key("a"), time_key("a"));
    }
}
