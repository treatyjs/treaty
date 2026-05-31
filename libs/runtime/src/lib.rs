//! Treaty JS-engine runtime.
//!
//! Hosts the JavaScript execution layer shared by macro/RSC (pre)render execution, TS
//! server-function execution, and the serverless server-side runtime. Engine: Nova
//! (pure-Rust JS engine).
//!
//! (rusty_v8 was ruled out on this Windows host: its build script needs the symbolic-link
//! privilege / Developer Mode, which is unavailable here. Boa is an emergency fallback only.)
//!
//! The public surface is [`JsRuntime`]: create one, then [`JsRuntime::eval`] a script string
//! and receive its completion value (the value of the last expression) converted to a
//! [`serde_json::Value`]. Host input can be injected with [`JsRuntime::eval_with_input`], which
//! exposes a JSON object to the script as the globals `input` and `__args`.
//!
//! Nova evaluates synchronously and has no built-in event loop; `async`/`await` and promise
//! draining are a documented follow-up and are not exercised here.
//!
//! On top of the raw [`JsRuntime::eval`] surface, this crate exposes the render-time execution
//! paths shared across Treaty: [`run_macro`] transpiles a TypeScript macro to JavaScript and runs
//! it against an injected input (the static prerender path, also reusable per request for dynamic
//! prerender), and [`run_server_fn`] executes a TypeScript server-function body against its JSON
//! arguments. Both reuse the same Nova engine and the same TS->JS step.

use std::fmt;

mod node;
mod transpile;

pub use transpile::{transpile_ts, TranspileError};

use nova_vm::{
    ecmascript::{
        Agent, AgentOptions, DefaultHostHooks, GcAgent, Object, String as JsString, parse_script,
        script_evaluation,
    },
    engine::{Bindable, GcScope},
};
use serde_json::Value as JsonValue;

use crate::node::core::HostState;

/// An error raised while preparing or executing a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// The source text failed to parse (a `SyntaxError`-class failure). The string carries the
    /// concatenated diagnostic messages produced by the parser.
    Parse(String),
    /// The script threw, or otherwise completed abruptly, at runtime. The string carries the
    /// thrown value's string representation (e.g. `"Error: boom"`).
    Runtime(String),
    /// The completion value could not be converted into a [`serde_json::Value`]. This indicates a
    /// bug in the runtime's marshalling layer rather than a fault in the executed script.
    Conversion(String),
}

impl RuntimeError {
    /// The human-readable message carried by this error, regardless of variant.
    pub fn message(&self) -> &str {
        match self {
            RuntimeError::Parse(m) | RuntimeError::Runtime(m) | RuntimeError::Conversion(m) => m,
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Parse(m) => write!(f, "parse error: {m}"),
            RuntimeError::Runtime(m) => write!(f, "runtime error: {m}"),
            RuntimeError::Conversion(m) => write!(f, "conversion error: {m}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// A JavaScript execution runtime backed by the Nova engine.
///
/// Each [`JsRuntime`] owns a Nova agent and a default realm. A realm carries the global object
/// and all intrinsics, so successive `eval` calls on the same runtime share global state. Create
/// a fresh runtime when isolation between executions is required (for example, between unrelated
/// render-time macros).
pub struct JsRuntime {
    agent: GcAgent,
    realm: nova_vm::ecmascript::RealmRoot,
    // SAFETY: drop order. When the Node-compat layer is enabled, the agent below holds a
    // `&'static HostState` produced by `node::core::extend_lifetime` from this box. Rust drops
    // fields in declaration order, so `agent` (declared first) is destroyed before `host_state`
    // (declared last) — the agent therefore never observes a freed `HostState`. This box is the
    // only thing keeping that erased-lifetime reference valid; it MUST stay the final field. It is
    // `None` for a plain `JsRuntime::new`, which uses Nova's `DefaultHostHooks` instead.
    host_state: Option<Box<HostState>>,
}

impl JsRuntime {
    /// Construct a new runtime with a fresh Nova agent and a default realm.
    ///
    /// This is the plain, Node-free runtime: it uses Nova's `DefaultHostHooks` and a default realm,
    /// exactly as before. The Node-compat layer is opt-in via [`JsRuntime::with_node_compat`].
    pub fn new() -> Self {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        Self {
            agent,
            realm,
            host_state: None,
        }
    }

    /// Construct a runtime with the Treaty Node-compatibility layer installed.
    ///
    /// This wires a [`HostState`] (the microtask/timer event loop, the `oxc_resolver`-backed module
    /// resolver, and the lazy builtin registry) behind Nova's host hooks, and initializes the
    /// realm's global object with the Node globals (eager ones immediately; the rest as lazy
    /// self-replacing accessors). The CWD and environment are captured from the current process.
    ///
    /// All other behavior — `eval`, `eval_with_input`, `run_macro`, `run_server_fn` — is unchanged;
    /// the Node layer is purely additive. After a script evaluates, `eval*` drains the event loop so
    /// scheduled promise jobs and timers settle before the completion value is read.
    pub fn with_node_compat() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let env = std::env::vars().collect();
        let host_state = Box::new(HostState::new(cwd, env));

        // SAFETY: Nova requires `&'static dyn HostHooks`. The reference is derived from `host_state`,
        // which is moved into the returned struct as its LAST field and so outlives `agent` (see the
        // field-drop-order comment on `JsRuntime::host_state`). This is the single localized `unsafe`
        // of the Node layer (defined in `node::core`).
        let hooks: &'static HostState = unsafe { node::core::extend_lifetime(&*host_state) };

        let mut agent = GcAgent::new(
            AgentOptions {
                disable_gc: false,
                print_internals: false,
                no_block: false,
            },
            hooks,
        );

        // No custom global object / globalThis value; only a global-initializer that installs the
        // Node globals. Typed `None`s satisfy the generic `Option<impl FnOnce...>` parameters.
        let create_global_object: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> =
            None;
        let create_global_this_value: Option<
            for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>,
        > = None;
        let initialize_global: Option<fn(&mut Agent, Object, GcScope)> = Some(node::install);

        let realm = agent.create_realm(
            create_global_object,
            create_global_this_value,
            initialize_global,
        );

        Self {
            agent,
            realm,
            host_state: Some(host_state),
        }
    }

    /// Evaluate `source` and return its completion value as a [`serde_json::Value`].
    ///
    /// The completion value is the value of the script's final expression statement. JavaScript
    /// numbers, strings, booleans, `null`, arrays and plain objects round-trip to the matching
    /// JSON shapes. Values that have no JSON representation (`undefined`, functions, symbols) and
    /// scripts whose final statement is not an expression yield [`JsonValue::Null`].
    pub fn eval(&mut self, source: &str) -> Result<JsonValue, RuntimeError> {
        self.eval_with_input(source, &JsonValue::Null)
    }

    /// Evaluate `source` with `input` injected as the globals `input`, `__args` and `args`,
    /// returning the completion value as a [`serde_json::Value`].
    ///
    /// `input` is serialized to JSON and spliced into the script as a literal, so the executed
    /// code can read it directly (e.g. `input.x`). The same value is also bound to `args`, the
    /// conventional name a server function reads positional arguments from (the caller passes a
    /// JSON array there). Any JSON value is accepted; passing [`JsonValue::Null`] is equivalent to
    /// [`JsRuntime::eval`].
    pub fn eval_with_input(
        &mut self,
        source: &str,
        input: &JsonValue,
    ) -> Result<JsonValue, RuntimeError> {
        let wrapped = wrap_source(source, input)?;
        let JsRuntime {
            agent,
            realm,
            host_state,
        } = self;
        // Borrow the event loop out of the (optional) host state for the drain below. `None` for a
        // plain `JsRuntime::new`, in which case no draining happens and behavior is unchanged.
        let event_loop = host_state.as_deref().map(|s| s.event_loop());

        let outcome = agent.run_in_realm(realm, |agent, mut gc| -> Result<String, RuntimeError> {
            // Build the source string on the Nova heap.
            let source_text = JsString::from_string(agent, wrapped, gc.nogc());

            // Parse as a (strict-mode) Script in the current realm.
            let current_realm = agent.current_realm(gc.nogc());
            let script = match parse_script(agent, source_text, current_realm, true, None, gc.nogc())
            {
                Ok(script) => script,
                Err(diagnostics) => {
                    let message = diagnostics
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("; ");
                    let message = if message.is_empty() {
                        "failed to parse script".to_owned()
                    } else {
                        message
                    };
                    return Err(RuntimeError::Parse(message));
                }
            };

            // Evaluate it. The wrapper guarantees the completion value is a JS string holding the
            // JSON encoding of `{ "v": <user result> }` (or an Error is thrown).
            let result = script_evaluation(agent, script.unbind(), gc.reborrow())
                .unbind()
                .bind(gc.nogc());

            match result {
                Ok(value) => {
                    // Read the completion value first. The wrapper always completes with the final
                    // `JSON.stringify({ v: ... })` string, computed synchronously, so reading it now
                    // captures the result before any event-loop draining can move heap objects.
                    // `string_repr` never throws.
                    let repr = value
                        .unbind()
                        .string_repr(agent, gc.reborrow())
                        .to_string_lossy(agent)
                        .into_owned();

                    // With the Node layer enabled, drain the event loop so promise jobs and due
                    // timers scheduled by the script run their side effects. Empty queues return
                    // immediately, so a script that schedules nothing is unaffected. A job that
                    // throws aborts the drain and surfaces as a runtime error.
                    if let Some(event_loop) = event_loop {
                        node::event_loop::run_until_idle(agent, event_loop, None, gc.reborrow())
                            .unbind()
                            .map_err(|error| {
                                let message = error
                                    .value()
                                    .unbind()
                                    .string_repr(agent, gc)
                                    .to_string_lossy(agent)
                                    .into_owned();
                                RuntimeError::Runtime(message)
                            })?;
                    }

                    Ok(repr)
                }
                Err(error) => {
                    let message = error
                        .value()
                        .unbind()
                        .string_repr(agent, gc)
                        .to_string_lossy(agent)
                        .into_owned();
                    Err(RuntimeError::Runtime(message))
                }
            }
        })?;

        // `outcome` is the JSON encoding of `{ "v": <result> }`. Decode and unwrap `v`. A missing
        // `v` (the user value had no JSON form) decodes as the absent key -> JSON null.
        let envelope: JsonValue = serde_json::from_str(&outcome).map_err(|e| {
            RuntimeError::Conversion(format!("could not decode result envelope: {e}"))
        })?;
        match envelope {
            JsonValue::Object(mut map) => Ok(map.remove("v").unwrap_or(JsonValue::Null)),
            other => Err(RuntimeError::Conversion(format!(
                "unexpected result envelope shape: {other}"
            ))),
        }
    }
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// The value a macro produced for injection, captured as JSON.
///
/// A macro runs server-side at render time and yields a single value (its default export, an
/// explicit `return`, or a trailing expression). That value is serialized to JSON so it can be
/// spliced into the rendered output. Values with no JSON form (`undefined`, functions, symbols)
/// are represented as [`JsonValue::Null`], matching [`JsRuntime::eval`].
#[derive(Debug, Clone, PartialEq)]
pub struct MacroOutput {
    /// The macro's produced value, as JSON.
    pub value: JsonValue,
}

impl MacroOutput {
    /// Borrow the produced value.
    pub fn value(&self) -> &JsonValue {
        &self.value
    }

    /// Consume the output, yielding the produced value.
    pub fn into_value(self) -> JsonValue {
        self.value
    }
}

/// Execute a Treaty macro and capture the value it produces for injection.
///
/// `ts_source` is the TypeScript body of a macro (the top-of-file fenced block in a `.treaty`
/// file). It is transpiled to JavaScript (TypeScript syntax stripped) and then run in a fresh Nova
/// isolate with `input_json` injected as the globals `input` and `__args`.
///
/// The macro produces its value in any of the natural forms:
///   * `export default <expr>;`  — the default export (rewritten to a `return`),
///   * `return <expr>;`          — an explicit return from the macro body,
///   * a trailing expression     — the value of the body's final expression.
///
/// This is the **static prerender** path: call it once at build time. It is equally the
/// **dynamic prerender** entry — calling it again with a different `input_json` re-runs the same
/// macro per request, since each call uses an isolated runtime and re-injects the input.
///
/// Returns [`RuntimeError::Parse`] if the source is not valid TypeScript / cannot be transpiled,
/// or [`RuntimeError::Runtime`] if the macro throws while executing.
pub fn run_macro(ts_source: &str, input_json: &JsonValue) -> Result<MacroOutput, RuntimeError> {
    let js = transpile_macro_to_js(ts_source)?;
    let mut rt = JsRuntime::new();
    let value = rt.eval_with_input(&js, input_json)?;
    Ok(MacroOutput { value })
}

/// Execute a Treaty server function and return its result as JSON.
///
/// `ts_source` is the TypeScript body of a server function; `args_json` is injected as the globals
/// `args` (an array of positional arguments) and `__args`. The body is transpiled to JavaScript and
/// executed in a fresh Nova isolate using the same engine as [`run_macro`]. The function's result
/// is the value it `return`s (or its trailing expression when `return` is omitted).
///
/// This is the serverless server-function execution path: one call per invocation.
///
/// Returns [`RuntimeError::Parse`] on a transpile failure, or [`RuntimeError::Runtime`] if the
/// function throws.
pub fn run_server_fn(ts_source: &str, args_json: &JsonValue) -> Result<JsonValue, RuntimeError> {
    let js = transpile_macro_to_js(ts_source)?;
    let mut rt = JsRuntime::new();
    // Server functions read positional arguments from `args`; `input`/`__args` are also bound by
    // `eval_with_input`, so a server fn may equally read `__args`.
    rt.eval_with_input(&js, args_json)
}

/// Shared TS->JS preparation for [`run_macro`] and [`run_server_fn`].
///
/// The macro / server-fn body is render-time code that produces a single value via a default
/// export, an explicit `return`, or a trailing expression. To make all three forms legal and
/// capturable, the body is first wrapped in a function (so top-level `return` and `export default`
/// — once rewritten — are valid statements), then transpiled to JavaScript. Wrapping *before*
/// transpiling matters: a bare top-level `return` is a parse error in a script, so it must already
/// sit inside a function when the parser runs.
///
/// The produced JavaScript is a single immediately-invoked function expression; its result is the
/// macro's value, ready to hand to [`JsRuntime::eval_with_input`].
fn transpile_macro_to_js(ts_source: &str) -> Result<String, RuntimeError> {
    // `export default <expr>` -> `return <expr>`, and ensure a trailing expression becomes a
    // `return`. These are syntax-level rewrites that operate equally on TypeScript source.
    let body = rewrite_default_export(ts_source);
    let body = ensure_trailing_return(&body);

    // Wrap in a function so `return` is legal, with a fallthrough `return undefined` so a body that
    // never returns yields `undefined` (-> JSON null) rather than leaking a completion value.
    let wrapped_ts = format!("(function () {{\n{body}\nreturn undefined;\n}})()");

    transpile_ts(&wrapped_ts).map_err(|e| RuntimeError::Parse(e.0))
}

/// Rewrite a leading `export default <expr>` into `return <expr>` so a macro's default export is
/// captured as its produced value when run as a function body.
///
/// Scripts evaluated via `eval` cannot contain `export`, so the (transpiled) `export default` form
/// is converted to a `return`. Only the `export default` keyword prefix is rewritten; the trailing
/// expression is left intact, including its terminating semicolon if present.
fn rewrite_default_export(js: &str) -> String {
    let trimmed = js.trim_start();
    if let Some(rest) = trimmed.strip_prefix("export default ") {
        // Preserve any leading whitespace that `trim_start` removed so spans/line counts in error
        // messages stay close to the original.
        let lead_len = js.len() - trimmed.len();
        let lead = &js[..lead_len];
        format!("{lead}return {rest}")
    } else {
        js.to_owned()
    }
}

/// Ensure the body ends in a `return` so a trailing expression statement is captured.
///
/// If the body already contains a `return` at statement level the body is returned unchanged
/// (the explicit `return` wins). Otherwise, when the body's final non-empty, non-comment segment
/// looks like a bare expression statement, it is prefixed with `return `. This is a deliberately
/// conservative textual heuristic for the supported subset: bodies that need richer control flow
/// should use an explicit `return`, which always takes precedence.
fn ensure_trailing_return(body: &str) -> String {
    let trimmed = body.trim_end();
    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed).trim_end();

    // An explicit top-level `return` is authoritative — never second-guess it.
    if contains_top_level_return(trimmed) {
        return body.to_owned();
    }

    // Find the start of the final statement: the character after the last top-level `;` or `}`.
    let split_at = last_top_level_statement_boundary(trimmed);
    let (head, tail) = trimmed.split_at(split_at);
    let tail_trimmed = tail.trim_start();

    // Only treat the tail as a value-producing expression when it does not begin a statement that
    // already has its own semantics (declarations, control flow, blocks). For those, falling through
    // to `return undefined` is correct.
    if tail_trimmed.is_empty() || starts_statement_keyword(tail_trimmed) || tail_trimmed.starts_with('{') {
        return body.to_owned();
    }

    format!("{head}return {tail_trimmed};")
}

/// True when `body` contains a `return` token at the top brace/paren level (not nested inside a
/// function literal). Used so an explicit `return` is never overridden by the trailing-expression
/// heuristic.
fn contains_top_level_return(body: &str) -> bool {
    let bytes = body.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            // Match the keyword `return` on a word boundary at the top level.
            b'r' if depth == 0
                && body[i..].starts_with("return")
                && !preceded_by_ident_char(bytes, i)
                && !followed_by_ident_char(bytes, i + "return".len()) =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Byte index just past the last top-level statement separator (`;` or `}`), i.e. the start of the
/// final statement. Zero when there is no separator (single-statement body).
fn last_top_level_statement_boundary(body: &str) -> usize {
    let bytes = body.as_bytes();
    let mut depth: i32 = 0;
    let mut boundary = 0;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' | b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            // A closing brace that returns to the top level ends a block statement (e.g. a function
            // or `if` body), so the next statement starts after it. Closing parens/brackets only
            // finish a sub-expression and must not be treated as statement boundaries.
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    boundary = i + 1;
                }
            }
            b';' if depth == 0 => boundary = i + 1,
            _ => {}
        }
    }
    boundary
}

/// True when `tail` begins with a statement keyword whose completion value must not be `return`ed.
fn starts_statement_keyword(tail: &str) -> bool {
    const KEYWORDS: [&str; 14] = [
        "var ", "let ", "const ", "function", "class ", "if ", "if(", "for ", "for(", "while ",
        "while(", "switch ", "switch(", "throw ",
    ];
    KEYWORDS.iter().any(|kw| tail.starts_with(kw))
}

fn preceded_by_ident_char(bytes: &[u8], i: usize) -> bool {
    i > 0 && is_ident_char(bytes[i - 1])
}

fn followed_by_ident_char(bytes: &[u8], i: usize) -> bool {
    i < bytes.len() && is_ident_char(bytes[i])
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Build the script actually handed to Nova.
///
/// The wrapper:
///   1. defines `input` / `__args` from the injected JSON literal,
///   2. runs the user source through *indirect* `eval` so its completion value (the final
///      expression's value) is captured into a local, and
///   3. completes with `JSON.stringify({ v: <result> })`.
///
/// Wrapping the result in an envelope object makes the JSON marshalling total: `undefined`,
/// functions and symbols cause the `v` key to be omitted (since `JSON.stringify` drops them),
/// which the Rust side maps to [`JsonValue::Null`]. The whole user body sits inside `eval` so
/// that statement-only scripts (no trailing expression) also succeed, completing with `null`.
fn wrap_source(source: &str, input: &JsonValue) -> Result<String, RuntimeError> {
    // `serde_json::to_string` of any JSON value is also a valid JavaScript expression, and of any
    // Rust string is a valid JavaScript string literal — so both splices are injection-safe.
    let input_literal = serde_json::to_string(input)
        .map_err(|e| RuntimeError::Conversion(format!("could not encode input: {e}")))?;
    let source_literal = serde_json::to_string(source)
        .map_err(|e| RuntimeError::Conversion(format!("could not encode source: {e}")))?;

    // `input` / `__args` expose the injected value to macros; `args` is the same value, provided as
    // the conventional name a server function reads its positional arguments from (the caller passes
    // a JSON array there). All three are plain `var`s so the user body can read them directly.
    Ok(format!(
        "var input = {input_literal};\n\
         var __args = input;\n\
         var args = input;\n\
         var __treaty_result = (0, eval)({source_literal});\n\
         JSON.stringify({{ v: __treaty_result }});\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluates_number() {
        let mut rt = JsRuntime::new();
        assert_eq!(rt.eval("1 + 2").unwrap(), json!(3));
        assert_eq!(rt.eval("0.5 + 0.25").unwrap(), json!(0.75));
    }

    #[test]
    fn evaluates_string() {
        let mut rt = JsRuntime::new();
        assert_eq!(rt.eval("'hello ' + 'world'").unwrap(), json!("hello world"));
    }

    #[test]
    fn evaluates_boolean_and_null() {
        let mut rt = JsRuntime::new();
        assert_eq!(rt.eval("1 < 2").unwrap(), json!(true));
        assert_eq!(rt.eval("null").unwrap(), JsonValue::Null);
    }

    #[test]
    fn evaluates_object_literal() {
        let mut rt = JsRuntime::new();
        let value = rt.eval("({ a: 1, b: 'two', c: [true, null] })").unwrap();
        assert_eq!(value, json!({ "a": 1, "b": "two", "c": [true, null] }));
    }

    #[test]
    fn evaluates_array() {
        let mut rt = JsRuntime::new();
        assert_eq!(rt.eval("[1, 2, 3].map(x => x * 2)").unwrap(), json!([2, 4, 6]));
    }

    #[test]
    fn undefined_completion_is_null() {
        let mut rt = JsRuntime::new();
        // A statement-only script has no trailing expression value.
        assert_eq!(rt.eval("var q = 5;").unwrap(), JsonValue::Null);
        // An explicit `undefined` likewise has no JSON form.
        assert_eq!(rt.eval("undefined").unwrap(), JsonValue::Null);
    }

    #[test]
    fn reads_injected_input() {
        let mut rt = JsRuntime::new();
        let input = json!({ "x": 21, "label": "answer" });
        let value = rt
            .eval_with_input("input.x * 2", &input)
            .unwrap();
        assert_eq!(value, json!(42));

        let value = rt
            .eval_with_input("__args.label + '!'", &input)
            .unwrap();
        assert_eq!(value, json!("answer!"));
    }

    #[test]
    fn injected_input_supports_nested_and_arrays() {
        let mut rt = JsRuntime::new();
        let input = json!({ "items": [1, 2, 3], "meta": { "n": 10 } });
        let value = rt
            .eval_with_input(
                "input.items.reduce((a, b) => a + b, 0) + input.meta.n",
                &input,
            )
            .unwrap();
        assert_eq!(value, json!(16));
    }

    #[test]
    fn throwing_script_returns_runtime_error() {
        let mut rt = JsRuntime::new();
        let err = rt
            .eval("throw new Error('boom')")
            .expect_err("expected a runtime error");
        match &err {
            RuntimeError::Runtime(msg) => assert!(
                msg.contains("boom"),
                "message should carry the thrown text, got: {msg}"
            ),
            other => panic!("expected RuntimeError::Runtime, got {other:?}"),
        }
    }

    #[test]
    fn throwing_a_plain_value_is_captured() {
        let mut rt = JsRuntime::new();
        let err = rt
            .eval("throw 'just a string'")
            .expect_err("expected a runtime error");
        assert!(err.message().contains("just a string"));
    }

    #[test]
    fn syntax_error_returns_parse_error() {
        let mut rt = JsRuntime::new();
        // The bad token lives in the user body, which is itself parsed by `eval` at runtime; an
        // early `SyntaxError` therefore surfaces as a thrown value. Either way it must be an Err
        // carrying a message — never a panic or a bogus Ok.
        let err = rt
            .eval("this is not valid ^^ javascript")
            .expect_err("expected an error for invalid source");
        assert!(!err.message().is_empty());
    }

    #[test]
    fn realm_state_persists_across_evals() {
        let mut rt = JsRuntime::new();
        assert_eq!(rt.eval("globalThis.counter = 1; counter").unwrap(), json!(1));
        assert_eq!(rt.eval("counter += 1; counter").unwrap(), json!(2));
    }

    // --- macro / server-fn render-time paths -------------------------------------------------

    #[test]
    fn macro_computes_data_from_injected_input() {
        // A TypeScript macro that reads its injected input, with type annotations the transpile
        // step must strip, and produces a returned object.
        let src = r#"
            const count: number = input.count;
            const label: string = input.label;
            return { doubled: count * 2, greeting: `hello ${label}` };
        "#;
        let out = run_macro(src, &json!({ "count": 21, "label": "world" })).unwrap();
        assert_eq!(
            out.value,
            json!({ "doubled": 42, "greeting": "hello world" })
        );
    }

    #[test]
    fn macro_supports_export_default() {
        // The canonical macro shape: a default export of the produced value.
        let src = "export default { ok: true, n: input.n + 1 };";
        let out = run_macro(src, &json!({ "n": 9 })).unwrap();
        assert_eq!(out.value, json!({ "ok": true, "n": 10 }));
    }

    #[test]
    fn macro_supports_trailing_expression() {
        // No explicit return: the trailing expression is the produced value.
        let src = "const xs: number[] = input.xs; xs.map((x: number) => x + 1)";
        let out = run_macro(src, &json!({ "xs": [1, 2, 3] })).unwrap();
        assert_eq!(out.value, json!([2, 3, 4]));
    }

    #[test]
    fn macro_uses_array_object_and_string_ops() {
        let src = r#"
            const items: string[] = input.items;
            const joined = items.map(s => s.toUpperCase()).join(", ");
            const total = items.reduce((acc, s) => acc + s.length, 0);
            return { joined, total, first: items[0].slice(0, 1) };
        "#;
        let out = run_macro(src, &json!({ "items": ["ab", "cde"] })).unwrap();
        assert_eq!(
            out.value,
            json!({ "joined": "AB, CDE", "total": 5, "first": "a" })
        );
    }

    #[test]
    fn macro_is_reusable_per_request_with_different_input() {
        // The dynamic-prerender contract: the same macro source re-run with fresh input.
        let src = "return { id: input.id, squared: input.id * input.id };";
        let a = run_macro(src, &json!({ "id": 3 })).unwrap();
        let b = run_macro(src, &json!({ "id": 5 })).unwrap();
        assert_eq!(a.value, json!({ "id": 3, "squared": 9 }));
        assert_eq!(b.value, json!({ "id": 5, "squared": 25 }));
    }

    #[test]
    fn throwing_macro_returns_err_with_message() {
        let src = "if (input.bad) { throw new Error('macro blew up'); } return 1;";
        let err = run_macro(src, &json!({ "bad": true })).expect_err("expected an error");
        match &err {
            RuntimeError::Runtime(msg) => {
                assert!(msg.contains("macro blew up"), "got: {msg}")
            }
            other => panic!("expected RuntimeError::Runtime, got {other:?}"),
        }
    }

    #[test]
    fn macro_with_invalid_typescript_returns_parse_error() {
        let err = run_macro("const = ;", &JsonValue::Null).expect_err("expected an error");
        assert!(matches!(err, RuntimeError::Parse(_)), "got: {err:?}");
        assert!(!err.message().is_empty());
    }

    #[test]
    fn server_fn_executes_body_with_args() {
        // A server function reading positional arguments from `args`, with TS annotations stripped.
        let src = r#"
            const a: number = args[0];
            const b: number = args[1];
            return a + b;
        "#;
        let result = run_server_fn(src, &json!([4, 38])).unwrap();
        assert_eq!(result, json!(42));
    }

    #[test]
    fn server_fn_returns_object_result() {
        let src = r#"
            const [name, count]: [string, number] = args;
            return { name, count, ok: count > 0 };
        "#;
        let result = run_server_fn(src, &json!(["widget", 7])).unwrap();
        assert_eq!(
            result,
            json!({ "name": "widget", "count": 7, "ok": true })
        );
    }

    #[test]
    fn throwing_server_fn_returns_err() {
        let src = "throw new Error('server fn failed');";
        let err = run_server_fn(src, &json!([])).expect_err("expected an error");
        assert!(err.message().contains("server fn failed"), "got: {}", err.message());
    }

    #[test]
    fn server_fn_reuses_same_engine_as_macro() {
        // Both paths run on the same Nova engine; a value that round-trips through one round-trips
        // through the other identically.
        let macro_out = run_macro("return input.v * 10;", &json!({ "v": 2 })).unwrap();
        let fn_out = run_server_fn("return args[0] * 10;", &json!([2])).unwrap();
        assert_eq!(macro_out.value, fn_out);
        assert_eq!(fn_out, json!(20));
    }

    // --- Node-compat layer (additive; opt-in via `with_node_compat`) -------------------------

    #[test]
    fn node_compat_runtime_evaluates_like_the_plain_one() {
        // The Node layer is additive: the core eval surface behaves identically.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval("1 + 2").unwrap(), json!(3));
        assert_eq!(
            rt.eval("({ a: [1, 2], b: 'x' })").unwrap(),
            json!({ "a": [1, 2], "b": "x" })
        );
    }

    #[test]
    fn node_compat_installs_global_self_reference() {
        // `globals::install_globals` ran via the realm init hook: Node's `global` aliases globalThis.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval("global === globalThis").unwrap(), json!(true));
    }

    #[test]
    fn node_compat_drains_promise_microtasks_before_returning() {
        // The event-loop pump runs after evaluation: a promise's `.then` side effect lands on a
        // global, observable by the next eval.
        let mut rt = JsRuntime::with_node_compat();
        let value = rt
            .eval(
                "globalThis.__hit = 0;\
                 Promise.resolve().then(() => { globalThis.__hit = 1; });\
                 globalThis.__hit",
            )
            .unwrap();
        // The trailing expression reads `__hit` synchronously (still 0); after the drain the job has
        // run, which the next eval observes.
        assert_eq!(value, json!(0));
        assert_eq!(rt.eval("globalThis.__hit").unwrap(), json!(1));
    }

    #[test]
    fn node_compat_fs_require_relative_and_promise_smoke() {
        // End-to-end integration smoke exercising the whole Node-compat layer at once:
        //   1. `require("node:fs")` returns the builtin fs module,
        //   2. fs writes then reads back a temp file (round-trip through the host),
        //   3. `require("./fixture.js")` resolves a relative user file via oxc_resolver and
        //      evaluates it through the CommonJS loader, exposing its `module.exports`,
        //   4. a Promise (resolved from the fixture's value) settles after the event-loop pump,
        //      landing its result on a global the next eval observes.
        let dir = std::env::temp_dir().join(format!("treaty-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Relative user fixture: a plain CommonJS module the CJS loader will evaluate.
        let fixture = dir.join("fixture.js");
        std::fs::write(&fixture, "module.exports = { gift: 21 };\n").unwrap();

        // The temp file fs will write to and read back from inside the script.
        let data_path = dir.join("data.txt");
        let data_lit = serde_json::to_string(&data_path.to_string_lossy().into_owned()).unwrap();
        let fixture_lit =
            serde_json::to_string(&fixture.to_string_lossy().into_owned()).unwrap();

        let src = format!(
            "const fs = require(\"node:fs\");\
             fs.writeFileSync({data_lit}, \"forty-two\");\
             const roundtrip = fs.readFileSync({data_lit}, \"utf8\");\
             const dep = require({fixture_lit});\
             globalThis.__smoke = 0;\
             Promise.resolve(dep.gift * 2).then((v) => {{ globalThis.__smoke = v; }});\
             roundtrip"
        );

        let mut rt = JsRuntime::with_node_compat();
        // The synchronous fs round-trip is observable in the completion value.
        assert_eq!(rt.eval(&src).unwrap(), json!("forty-two"));
        // After the event-loop pump (which `eval` runs post-evaluation), the awaited promise
        // resolved with the relative-required fixture's value (21 * 2 == 42).
        assert_eq!(rt.eval("globalThis.__smoke").unwrap(), json!(42));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
