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

use std::fmt;

use nova_vm::{
    ecmascript::{DefaultHostHooks, GcAgent, String as JsString, parse_script, script_evaluation},
    engine::Bindable,
};
use serde_json::Value as JsonValue;

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
}

impl JsRuntime {
    /// Construct a new runtime with a fresh Nova agent and a default realm.
    pub fn new() -> Self {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        Self { agent, realm }
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

    /// Evaluate `source` with `input` injected as the globals `input` and `__args`, returning the
    /// completion value as a [`serde_json::Value`].
    ///
    /// `input` is serialized to JSON and spliced into the script as a literal, so the executed
    /// code can read it directly (e.g. `input.x`). Any JSON value is accepted; passing
    /// [`JsonValue::Null`] is equivalent to [`JsRuntime::eval`].
    pub fn eval_with_input(
        &mut self,
        source: &str,
        input: &JsonValue,
    ) -> Result<JsonValue, RuntimeError> {
        let wrapped = wrap_source(source, input)?;
        let JsRuntime { agent, realm } = self;

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
                    // `string_repr` never throws; the wrapper always completes with a string.
                    let repr = value
                        .unbind()
                        .string_repr(agent, gc)
                        .to_string_lossy(agent)
                        .into_owned();
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

    Ok(format!(
        "var input = {input_literal};\n\
         var __args = input;\n\
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
}
