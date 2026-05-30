//! `@defer` block + trigger parsing: the render3 parser for Angular control-flow
//! `@defer` blocks (and their connected `@placeholder`/`@loading`/`@error` blocks)
//! plus the trigger micro-syntax (`on idle`, `on timer(500)`, `when cond`,
//! `prefetch on …`, `hydrate on …`, `hydrate never`).
//!
//! PORT TARGET: `migration/render3-specs/09-deferred.md`
//! Sources (Angular 22.1.0-next.0, `packages/compiler/src/render3`):
//! - `r3_deferred_blocks.ts`
//! - `r3_deferred_triggers.ts`
//!
//! This module runs at *parse* time (html-AST → render3 template-AST). It emits no
//! `ɵɵ` instructions; it only constructs [`crate::template::r3_ast`] `Deferred*` nodes.
//!
//! ## Span bridging
//!
//! The input [`crate::ml_parser`] AST carries the rich `ml_parser::ParseSourceSpan`
//! (with `ParseLocation`/`move_by`), while the output [`r3_ast`] nodes carry the
//! offset-only [`crate::expression::ast::ParseSourceSpan`] (`{ start, end }`). We do
//! all the `moveBy` arithmetic on the ml_parser spans (faithful line/col semantics)
//! and convert to the offset-only span via [`to_r3_span`] when building r3 nodes.
//!
//! ## The transform callback
//!
//! In TS, `createDeferredBlock` calls `html.visitAll(visitor, children, children)` to
//! recursively transform the block's HTML children into render3 template nodes. The
//! html→render3 transform driver (`r3_template_transform`) is a not-yet-ported sibling
//! (NOTE(port)), so this module takes the child-transform as a [`ChildTransform`]
//! callback, which the driver supplies. Tests pass a trivial transform.

use crate::expression::ast::{
    AstNode, AstWithSource, ExprKind, LiteralMapKey, ParseSourceSpan as R3Span,
};
use crate::expression::parser::Parser as ExprParser;
use crate::ml_parser::{
    Block as HtmlBlock, BlockParameter as HtmlBlockParameter,
    ParseSourceSpan as MlSpan, ParseError as MlParseError, Node as HtmlNode,
};
use crate::template::r3_ast as t;

// ---------------------------------------------------------------------------
// Char constants (mirror `../chars`).
// ---------------------------------------------------------------------------

const CHAR_LBRACE: u32 = b'{' as u32;
const CHAR_RBRACE: u32 = b'}' as u32;
const CHAR_LBRACKET: u32 = b'[' as u32;
const CHAR_RBRACKET: u32 = b']' as u32;
const CHAR_LPAREN: u32 = b'(' as u32;
const CHAR_RPAREN: u32 = b')' as u32;
const CHAR_COMMA: u32 = b',' as u32;

// ---------------------------------------------------------------------------
// Span helpers.
// ---------------------------------------------------------------------------

/// Convert an ml_parser span (rich, line/col) to the offset-only r3/expression span.
fn to_r3_span(span: &MlSpan) -> R3Span {
    R3Span {
        start: span.start.offset as u32,
        end: span.end.offset as u32,
    }
}

/// `new ParseSourceSpan(start, end)` from two ml_parser locations, then converted.
/// We build the ml_parser span first (so the `move_by` line/col bookkeeping stays
/// faithful) and downcast to the offset-only form.
fn r3_span_between(start: &crate::ml_parser::ParseLocation, end: &crate::ml_parser::ParseLocation) -> R3Span {
    R3Span {
        start: start.offset as u32,
        end: end.offset as u32,
    }
}

// ---------------------------------------------------------------------------
// The child-transform callback (stand-in for `html.visitAll(visitor, ...)`).
// ---------------------------------------------------------------------------

/// Transforms a slice of html-AST children into render3 template nodes. Supplied by
/// the (not-yet-ported) `r3_template_transform` driver. NOTE(port): real driver lives
/// in `template_transform.rs`; this signature mirrors `html.visitAll(visitor, children,
/// children)` collapsed to a plain transform.
pub type ChildTransform<'a> = dyn FnMut(&[HtmlNode]) -> Vec<t::Node> + 'a;

// ---------------------------------------------------------------------------
// Local "throw"-equivalent for the connected-block / factory code paths.
// ---------------------------------------------------------------------------

/// Mirrors a thrown `Error(message)` inside the `parse*Block` / `create*Trigger`
/// helpers — caught and converted to a [`MlParseError`] at the boundary.
type TriggerResult<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// Patterns (regex in TS; hand-rolled predicates here).
// ---------------------------------------------------------------------------

/// `/^\s$/` — a single whitespace char (used by `getTriggerParametersStart`).
fn is_separator_char(c: char) -> bool {
    c.is_whitespace()
}

/// Tests whether `expr` matches `/^<keyword>\s/` (keyword followed by whitespace).
fn starts_with_keyword_then_space(expr: &str, keyword: &str) -> bool {
    if let Some(rest) = expr.strip_prefix(keyword) {
        rest.chars().next().map(is_separator_char).unwrap_or(false)
    } else {
        false
    }
}

/// `/^prefetch\s+when\s/`, `/^hydrate\s+on\s/`, etc. — `<a>\s+<b>\s`.
fn starts_with_two_keywords(expr: &str, first: &str, second: &str) -> bool {
    let Some(rest) = expr.strip_prefix(first) else {
        return false;
    };
    // `\s+`
    let trimmed = rest.trim_start_matches(is_separator_char);
    if trimmed.len() == rest.len() {
        return false; // needed at least one space
    }
    starts_with_keyword_then_space(trimmed, second)
}

/// `/^hydrate\s+never(\s*)$/`.
fn is_hydrate_never(expr: &str) -> bool {
    let Some(rest) = expr.strip_prefix("hydrate") else {
        return false;
    };
    let trimmed = rest.trim_start_matches(is_separator_char);
    if trimmed.len() == rest.len() {
        return false; // `\s+`
    }
    let Some(after) = trimmed.strip_prefix("never") else {
        return false;
    };
    after.chars().all(is_separator_char)
}

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

/// `isConnectedDeferLoopBlock` — predicate for a block name connectable to `@defer`.
pub fn is_connected_defer_loop_block(name: &str) -> bool {
    name == "placeholder" || name == "loading" || name == "error"
}

/// The result of [`create_deferred_block`] — node plus accumulated recoverable errors.
pub struct DeferredBlockResult {
    pub node: t::DeferredBlock,
    pub errors: Vec<MlParseError>,
}

/// `createDeferredBlock` — builds a [`t::DeferredBlock`] from a `@defer` html block and
/// its connected blocks. Errors are accumulated, never thrown.
pub fn create_deferred_block(
    ast: &HtmlBlock,
    connected_blocks: &[HtmlBlock],
    transform: &mut ChildTransform,
    binding_parser: &ExprParser,
) -> DeferredBlockResult {
    let mut errors: Vec<MlParseError> = Vec::new();

    let ConnectedBlocks {
        placeholder,
        loading,
        error,
    } = parse_connected_blocks(connected_blocks, &mut errors, transform);

    let PrimaryTriggers {
        triggers,
        prefetch_triggers,
        hydrate_triggers,
    } = parse_primary_triggers(ast, binding_parser, &mut errors, placeholder.as_ref());

    // The `defer` block has a main span encompassing all of the connected branches.
    let mut last_end_source_span = ast.end_source_span.clone();
    let mut end_of_last_source_span = ast.source_span.end.clone();
    if let Some(last_connected) = connected_blocks.last() {
        last_end_source_span = last_connected.end_source_span.clone();
        end_of_last_source_span = last_connected.source_span.end.clone();
    }

    let source_span_with_connected =
        r3_span_between(&ast.source_span.start, &end_of_last_source_span);

    let children = transform(&ast.children);

    let node = t::DeferredBlock {
        children,
        triggers,
        prefetch_triggers,
        hydrate_triggers,
        placeholder,
        loading,
        error,
        main_block_span: to_r3_span(&ast.source_span),
        spans: t::BlockSpans {
            name_span: to_r3_span(&ast.name_span),
            source_span: source_span_with_connected,
            start_source_span: to_r3_span(&ast.start_source_span),
            end_source_span: last_end_source_span.as_ref().map(to_r3_span),
        },
        i18n: None,
    };

    DeferredBlockResult { node, errors }
}

// ---------------------------------------------------------------------------
// Connected blocks (@placeholder / @loading / @error).
// ---------------------------------------------------------------------------

struct ConnectedBlocks {
    placeholder: Option<t::DeferredBlockPlaceholder>,
    loading: Option<t::DeferredBlockLoading>,
    error: Option<t::DeferredBlockError>,
}

fn parse_connected_blocks(
    connected_blocks: &[HtmlBlock],
    errors: &mut Vec<MlParseError>,
    transform: &mut ChildTransform,
) -> ConnectedBlocks {
    let mut placeholder: Option<t::DeferredBlockPlaceholder> = None;
    let mut loading: Option<t::DeferredBlockLoading> = None;
    let mut error: Option<t::DeferredBlockError> = None;

    for block in connected_blocks {
        // `try { ... } catch (e) { push ParseError(block.startSourceSpan, e.message) }`.
        let outcome: TriggerResult<()> = (|| {
            if !is_connected_defer_loop_block(&block.name) {
                errors.push(MlParseError::new(
                    block.start_source_span.clone(),
                    format!("Unrecognized block \"@{}\"", block.name),
                ));
                // `break` — signal the caller to stop processing further blocks.
                return Err(BREAK_SENTINEL.to_string());
            }

            match block.name.as_str() {
                "placeholder" => {
                    if placeholder.is_some() {
                        errors.push(MlParseError::new(
                            block.start_source_span.clone(),
                            "@defer block can only have one @placeholder block".to_string(),
                        ));
                    } else {
                        placeholder = Some(parse_placeholder_block(block, transform)?);
                    }
                }
                "loading" => {
                    if loading.is_some() {
                        errors.push(MlParseError::new(
                            block.start_source_span.clone(),
                            "@defer block can only have one @loading block".to_string(),
                        ));
                    } else {
                        loading = Some(parse_loading_block(block, transform)?);
                    }
                }
                "error" => {
                    if error.is_some() {
                        errors.push(MlParseError::new(
                            block.start_source_span.clone(),
                            "@defer block can only have one @error block".to_string(),
                        ));
                    } else {
                        error = Some(parse_error_block(block, transform)?);
                    }
                }
                _ => {}
            }
            Ok(())
        })();

        match outcome {
            Ok(()) => {}
            Err(ref msg) if msg == BREAK_SENTINEL => break,
            Err(msg) => {
                errors.push(MlParseError::new(block.start_source_span.clone(), msg));
            }
        }
    }

    ConnectedBlocks {
        placeholder,
        loading,
        error,
    }
}

/// Sentinel "error" used purely to model the `break` after an unrecognized block,
/// distinct from a real thrown message.
const BREAK_SENTINEL: &str = "\0__defer_break__";

fn parse_placeholder_block(
    ast: &HtmlBlock,
    transform: &mut ChildTransform,
) -> TriggerResult<t::DeferredBlockPlaceholder> {
    let mut minimum_time: Option<f64> = None;

    for param in &ast.parameters {
        if starts_with_keyword_then_space(&param.expression, "minimum") {
            if minimum_time.is_some() {
                return Err("@placeholder block can only have one \"minimum\" parameter".to_string());
            }
            let start = get_trigger_parameters_start(&param.expression, 0);
            let slice = slice_from(&param.expression, start);
            match parse_deferred_time(slice) {
                Some(t) => minimum_time = Some(t),
                None => {
                    return Err("Could not parse time value of parameter \"minimum\"".to_string())
                }
            }
        } else {
            return Err(format!(
                "Unrecognized parameter in @placeholder block: \"{}\"",
                param.expression
            ));
        }
    }

    Ok(t::DeferredBlockPlaceholder {
        children: transform(&ast.children),
        minimum_time,
        spans: block_spans(ast),
        i18n: None,
    })
}

fn parse_loading_block(
    ast: &HtmlBlock,
    transform: &mut ChildTransform,
) -> TriggerResult<t::DeferredBlockLoading> {
    let mut after_time: Option<f64> = None;
    let mut minimum_time: Option<f64> = None;

    for param in &ast.parameters {
        if starts_with_keyword_then_space(&param.expression, "after") {
            if after_time.is_some() {
                return Err("@loading block can only have one \"after\" parameter".to_string());
            }
            let start = get_trigger_parameters_start(&param.expression, 0);
            match parse_deferred_time(slice_from(&param.expression, start)) {
                Some(t) => after_time = Some(t),
                None => return Err("Could not parse time value of parameter \"after\"".to_string()),
            }
        } else if starts_with_keyword_then_space(&param.expression, "minimum") {
            if minimum_time.is_some() {
                return Err("@loading block can only have one \"minimum\" parameter".to_string());
            }
            let start = get_trigger_parameters_start(&param.expression, 0);
            match parse_deferred_time(slice_from(&param.expression, start)) {
                Some(t) => minimum_time = Some(t),
                None => {
                    return Err("Could not parse time value of parameter \"minimum\"".to_string())
                }
            }
        } else {
            return Err(format!(
                "Unrecognized parameter in @loading block: \"{}\"",
                param.expression
            ));
        }
    }

    Ok(t::DeferredBlockLoading {
        children: transform(&ast.children),
        after_time,
        minimum_time,
        spans: block_spans(ast),
        i18n: None,
    })
}

fn parse_error_block(
    ast: &HtmlBlock,
    transform: &mut ChildTransform,
) -> TriggerResult<t::DeferredBlockError> {
    if !ast.parameters.is_empty() {
        return Err("@error block cannot have parameters".to_string());
    }
    Ok(t::DeferredBlockError {
        children: transform(&ast.children),
        spans: block_spans(ast),
        i18n: None,
    })
}

/// Build [`t::BlockSpans`] from an html block's positional spans.
fn block_spans(ast: &HtmlBlock) -> t::BlockSpans {
    t::BlockSpans {
        name_span: to_r3_span(&ast.name_span),
        source_span: to_r3_span(&ast.source_span),
        start_source_span: to_r3_span(&ast.start_source_span),
        end_source_span: ast.end_source_span.as_ref().map(to_r3_span),
    }
}

// ---------------------------------------------------------------------------
// Primary trigger dispatch.
// ---------------------------------------------------------------------------

struct PrimaryTriggers {
    triggers: t::DeferredBlockTriggers,
    prefetch_triggers: t::DeferredBlockTriggers,
    hydrate_triggers: t::DeferredBlockTriggers,
}

fn parse_primary_triggers(
    ast: &HtmlBlock,
    binding_parser: &ExprParser,
    errors: &mut Vec<MlParseError>,
    placeholder: Option<&t::DeferredBlockPlaceholder>,
) -> PrimaryTriggers {
    let mut triggers = t::DeferredBlockTriggers::default();
    let mut prefetch_triggers = t::DeferredBlockTriggers::default();
    let mut hydrate_triggers = t::DeferredBlockTriggers::default();

    for param in &ast.parameters {
        let expr = &param.expression;
        // The lexer ignores leading spaces; the expression starts with a keyword.
        if starts_with_keyword_then_space(expr, "when") {
            parse_when_trigger(param, binding_parser, &mut triggers, errors);
        } else if starts_with_keyword_then_space(expr, "on") {
            parse_on_trigger(param, binding_parser, &mut triggers, errors, placeholder);
        } else if starts_with_two_keywords(expr, "prefetch", "when") {
            parse_when_trigger(param, binding_parser, &mut prefetch_triggers, errors);
        } else if starts_with_two_keywords(expr, "prefetch", "on") {
            parse_on_trigger(param, binding_parser, &mut prefetch_triggers, errors, placeholder);
        } else if starts_with_two_keywords(expr, "hydrate", "when") {
            parse_when_trigger(param, binding_parser, &mut hydrate_triggers, errors);
        } else if starts_with_two_keywords(expr, "hydrate", "on") {
            parse_on_trigger(param, binding_parser, &mut hydrate_triggers, errors, placeholder);
        } else if is_hydrate_never(expr) {
            parse_never_trigger(param, &mut hydrate_triggers, errors);
        } else {
            errors.push(MlParseError::new(
                param.source_span.clone(),
                "Unrecognized trigger".to_string(),
            ));
        }
    }

    if hydrate_triggers.never.is_some() && hydrate_triggers.order.len() > 1 {
        errors.push(MlParseError::new(
            ast.start_source_span.clone(),
            "Cannot specify additional `hydrate` triggers if `hydrate never` is present".to_string(),
        ));
    }

    PrimaryTriggers {
        triggers,
        prefetch_triggers,
        hydrate_triggers,
    }
}

// ---------------------------------------------------------------------------
// `when` / `never` triggers.
// ---------------------------------------------------------------------------

/// `getPrefetchSpan` — span of the leading `prefetch` keyword, if present.
fn get_prefetch_span(expression: &str, source_span: &MlSpan) -> Option<R3Span> {
    if !expression.starts_with("prefetch") {
        return None;
    }
    Some(r3_span_between(
        &source_span.start,
        &source_span.start.move_by("prefetch".len() as isize),
    ))
}

/// `getHydrateSpan` — span of the leading `hydrate` keyword, if present.
fn get_hydrate_span(expression: &str, source_span: &MlSpan) -> Option<R3Span> {
    if !expression.starts_with("hydrate") {
        return None;
    }
    Some(r3_span_between(
        &source_span.start,
        &source_span.start.move_by("hydrate".len() as isize),
    ))
}

/// `parseNeverTrigger`. (The TS doc comment erroneously says "when"; this parses `never`.)
pub fn parse_never_trigger(
    param: &HtmlBlockParameter,
    triggers: &mut t::DeferredBlockTriggers,
    errors: &mut Vec<MlParseError>,
) {
    let expression = &param.expression;
    let source_span = &param.source_span;

    let never_index = char_index_of(expression, "never");
    let prefetch_span = get_prefetch_span(expression, source_span);
    let hydrate_span = get_hydrate_span(expression, source_span);

    match never_index {
        None => errors.push(MlParseError::new(
            source_span.clone(),
            "Could not find \"never\" keyword in expression".to_string(),
        )),
        Some(idx) => {
            let never_source_span = r3_span_between(
                &source_span.start.move_by(idx as isize),
                &source_span.start.move_by((idx + "never".len()) as isize),
            );
            let trigger = t::DeferredTrigger {
                kind: t::DeferredTriggerKind::Never,
                spans: t::TriggerSpans {
                    name_span: Some(never_source_span),
                    source_span: to_r3_span(source_span),
                    prefetch_span,
                    when_or_on_source_span: None,
                    hydrate_span,
                },
            };
            track_trigger(t::TriggerKey::Never, triggers, errors, trigger);
        }
    }
}

/// `parseWhenTrigger`.
pub fn parse_when_trigger(
    param: &HtmlBlockParameter,
    binding_parser: &ExprParser,
    triggers: &mut t::DeferredBlockTriggers,
    errors: &mut Vec<MlParseError>,
) {
    let expression = &param.expression;
    let source_span = &param.source_span;

    let when_index = char_index_of(expression, "when");
    let prefetch_span = get_prefetch_span(expression, source_span);
    let hydrate_span = get_hydrate_span(expression, source_span);

    match when_index {
        None => errors.push(MlParseError::new(
            source_span.clone(),
            "Could not find \"when\" keyword in expression".to_string(),
        )),
        Some(idx) => {
            let when_source_span = r3_span_between(
                &source_span.start.move_by(idx as isize),
                &source_span.start.move_by((idx + "when".len()) as isize),
            );
            let start = get_trigger_parameters_start(expression, idx + 1);
            let parsed = binding_parser.parse_binding(
                slice_from(expression, start),
                to_r3_span(source_span),
                (source_span.start.offset + start) as i32,
            );
            let trigger = t::DeferredTrigger {
                kind: t::DeferredTriggerKind::When {
                    value: aws_into_ast(parsed),
                },
                spans: t::TriggerSpans {
                    name_span: None,
                    source_span: to_r3_span(source_span),
                    prefetch_span,
                    when_or_on_source_span: Some(when_source_span),
                    hydrate_span,
                },
            };
            track_trigger(t::TriggerKey::When, triggers, errors, trigger);
        }
    }
}

/// `parseOnTrigger`.
pub fn parse_on_trigger(
    param: &HtmlBlockParameter,
    binding_parser: &ExprParser,
    triggers: &mut t::DeferredBlockTriggers,
    errors: &mut Vec<MlParseError>,
    _placeholder: Option<&t::DeferredBlockPlaceholder>,
) {
    let expression = &param.expression;
    let source_span = &param.source_span;

    let on_index = char_index_of(expression, "on");
    let prefetch_span = get_prefetch_span(expression, source_span);
    let hydrate_span = get_hydrate_span(expression, source_span);

    match on_index {
        None => errors.push(MlParseError::new(
            source_span.clone(),
            "Could not find \"on\" keyword in expression".to_string(),
        )),
        Some(idx) => {
            let on_source_span = r3_span_between(
                &source_span.start.move_by(idx as isize),
                &source_span.start.move_by((idx + "on".len()) as isize),
            );
            let start = get_trigger_parameters_start(expression, idx + 1);
            let is_hydration_trigger = expression.starts_with("hydrate");
            let validator = if is_hydration_trigger {
                ReferenceTriggerValidator::Hydrate
            } else {
                ReferenceTriggerValidator::Plain
            };
            let mut parser = OnTriggerParser::new(
                expression,
                binding_parser,
                start,
                source_span,
                triggers,
                errors,
                validator,
                is_hydration_trigger,
                prefetch_span,
                on_source_span,
                hydrate_span,
            );
            parser.parse();
        }
    }
}

// ---------------------------------------------------------------------------
// trackTrigger.
// ---------------------------------------------------------------------------

/// `trackTrigger` — inserts `trigger` into the slot for `key`, or pushes a duplicate
/// error if that slot is occupied. Records insertion order for traversal parity.
fn track_trigger(
    key: t::TriggerKey,
    all_triggers: &mut t::DeferredBlockTriggers,
    errors: &mut Vec<MlParseError>,
    trigger: t::DeferredTrigger,
) {
    let occupied = all_triggers.get(key).is_some();
    if occupied {
        // `trigger.sourceSpan` — already converted; wrap back into an ml-less ParseError
        // span. We keep the r3 offsets but ParseError wants an ml span; build a degenerate
        // one is impossible without a file handle, so we report via the offset span through
        // a synthetic ml span is not available — instead emit on the closest available.
        errors.push(duplicate_trigger_error(key, &trigger));
    } else {
        match key {
            t::TriggerKey::When => all_triggers.when = Some(trigger),
            t::TriggerKey::Idle => all_triggers.idle = Some(trigger),
            t::TriggerKey::Immediate => all_triggers.immediate = Some(trigger),
            t::TriggerKey::Hover => all_triggers.hover = Some(trigger),
            t::TriggerKey::Timer => all_triggers.timer = Some(trigger),
            t::TriggerKey::Interaction => all_triggers.interaction = Some(trigger),
            t::TriggerKey::Viewport => all_triggers.viewport = Some(trigger),
            t::TriggerKey::Never => all_triggers.never = Some(trigger),
        }
        all_triggers.push_order(key);
    }
}

/// The name used in `Duplicate "<name>" trigger is not allowed`.
fn trigger_key_name(key: t::TriggerKey) -> &'static str {
    match key {
        t::TriggerKey::When => "when",
        t::TriggerKey::Idle => "idle",
        t::TriggerKey::Immediate => "immediate",
        t::TriggerKey::Hover => "hover",
        t::TriggerKey::Timer => "timer",
        t::TriggerKey::Interaction => "interaction",
        t::TriggerKey::Viewport => "viewport",
        t::TriggerKey::Never => "never",
    }
}

/// A duplicate-trigger error. The TS code reports on `trigger.sourceSpan`; here we only
/// hold the offset-only r3 span, so we surface a [`MlParseError`] carrying the message
/// and the r3 offsets encoded into a synthetic-but-offset-faithful ml span via the
/// trigger's own span file is unavailable. We therefore carry offsets in the message-free
/// span by reusing the r3 offsets — see [`MlParseError`]; we build it from the r3 span's
/// offsets through [`offset_only_ml_span`].
fn duplicate_trigger_error(key: t::TriggerKey, trigger: &t::DeferredTrigger) -> MlParseError {
    MlParseError::new(
        offset_only_ml_span(&trigger.spans.source_span),
        format!(
            "Duplicate \"{}\" trigger is not allowed",
            trigger_key_name(key)
        ),
    )
}

// ---------------------------------------------------------------------------
// Trigger factories + validators.
// ---------------------------------------------------------------------------

/// `ReferenceTriggerValidator` — the two validator flavours (no closures).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceTriggerValidator {
    Plain,
    Hydrate,
}

impl ReferenceTriggerValidator {
    /// `validatePlainReferenceBasedTrigger` / `validateHydrateReferenceBasedTrigger`.
    fn validate(self, ty: OnTriggerType, parameters: &[ParsedParameter]) -> TriggerResult<()> {
        match self {
            ReferenceTriggerValidator::Plain => {
                if parameters.len() > 1 {
                    return Err(format!(
                        "\"{}\" trigger can only have zero or one parameters",
                        ty.as_str()
                    ));
                }
            }
            ReferenceTriggerValidator::Hydrate => {
                if ty == OnTriggerType::Viewport {
                    if parameters.len() > 1 {
                        return Err(format!(
                            "Hydration trigger \"{}\" cannot have more than one parameter",
                            ty.as_str()
                        ));
                    }
                    return Ok(());
                }
                if !parameters.is_empty() {
                    return Err(format!(
                        "Hydration trigger \"{}\" cannot have parameters",
                        ty.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// `OnTriggerType` — possible `on` trigger names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnTriggerType {
    Idle,
    Timer,
    Interaction,
    Immediate,
    Hover,
    Viewport,
    Never,
}

impl OnTriggerType {
    fn as_str(self) -> &'static str {
        match self {
            OnTriggerType::Idle => "idle",
            OnTriggerType::Timer => "timer",
            OnTriggerType::Interaction => "interaction",
            OnTriggerType::Immediate => "immediate",
            OnTriggerType::Hover => "hover",
            OnTriggerType::Viewport => "viewport",
            OnTriggerType::Never => "never",
        }
    }

    fn from_str(s: &str) -> Option<OnTriggerType> {
        Some(match s {
            "idle" => OnTriggerType::Idle,
            "timer" => OnTriggerType::Timer,
            "interaction" => OnTriggerType::Interaction,
            "immediate" => OnTriggerType::Immediate,
            "hover" => OnTriggerType::Hover,
            "viewport" => OnTriggerType::Viewport,
            "never" => OnTriggerType::Never,
            _ => return None,
        })
    }
}

/// `createIdleTrigger`.
fn create_idle_trigger(
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
) -> TriggerResult<t::DeferredTrigger> {
    if parameters.len() > 1 {
        return Err("\"idle\" trigger can only have zero or one parameters".to_string());
    }
    let mut timeout: Option<f64> = None;
    if let Some(p) = parameters.first() {
        match parse_deferred_time(&p.expression) {
            Some(t) => timeout = Some(t),
            None => return Err("Could not parse time value of trigger \"idle\"".to_string()),
        }
    }
    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Idle { timeout },
        spans,
    })
}

/// `createTimerTrigger`.
fn create_timer_trigger(
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
) -> TriggerResult<t::DeferredTrigger> {
    if parameters.len() != 1 {
        return Err("\"timer\" trigger must have exactly one parameter".to_string());
    }
    let delay = match parse_deferred_time(&parameters[0].expression) {
        Some(d) => d,
        None => return Err("Could not parse time value of trigger \"timer\"".to_string()),
    };
    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Timer { delay },
        spans,
    })
}

/// `createImmediateTrigger`.
fn create_immediate_trigger(
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
) -> TriggerResult<t::DeferredTrigger> {
    if !parameters.is_empty() {
        return Err("\"immediate\" trigger cannot have parameters".to_string());
    }
    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Immediate,
        spans,
    })
}

/// `createHoverTrigger`.
fn create_hover_trigger(
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
    validator: ReferenceTriggerValidator,
) -> TriggerResult<t::DeferredTrigger> {
    validator.validate(OnTriggerType::Hover, parameters)?;
    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Hover {
            reference: parameters.first().map(|p| p.expression.clone()),
        },
        spans,
    })
}

/// `createInteractionTrigger`.
fn create_interaction_trigger(
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
    validator: ReferenceTriggerValidator,
) -> TriggerResult<t::DeferredTrigger> {
    validator.validate(OnTriggerType::Interaction, parameters)?;
    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Interaction {
            reference: parameters.first().map(|p| p.expression.clone()),
        },
        spans,
    })
}

/// `createViewportTrigger`.
#[allow(clippy::too_many_arguments)]
fn create_viewport_trigger(
    start: usize,
    is_hydration_trigger: bool,
    binding_parser: &ExprParser,
    parameters: &[ParsedParameter],
    spans: t::TriggerSpans,
    source_span_for_binding: R3Span,
    binding_absolute_base: i32,
    validator: ReferenceTriggerValidator,
) -> TriggerResult<t::DeferredTrigger> {
    validator.validate(OnTriggerType::Viewport, parameters)?;

    let mut reference: Option<String> = None;
    let mut options: Option<AstNode> = None;

    if parameters.is_empty() {
        // both null
    } else if !parameters[0].expression.starts_with('{') {
        reference = Some(parameters[0].expression.clone());
    } else {
        let absolute_offset =
            binding_absolute_base + start as i32 + parameters[0].start as i32;
        let _ = source_span_for_binding;
        let parsed = binding_parser.parse_binding(
            &parameters[0].expression,
            source_span_for_binding,
            absolute_offset,
        );
        let ast = aws_into_ast(parsed);

        let ExprKind::LiteralMap { keys, values } = &ast.kind else {
            return Err(
                "Options parameter of the \"viewport\" trigger must be an object literal".to_string(),
            );
        };

        if keys.iter().any(|k| matches!(k, LiteralMapKey::Spread { .. })) {
            return Err("Spread operator are not allowed in this context".to_string());
        }
        if keys.iter().any(|k| property_key_is(k, "root")) {
            return Err(
                "The \"root\" option is not supported in the options parameter of the \"viewport\" trigger".to_string(),
            );
        }

        let trigger_index = keys.iter().position(|k| property_key_is(k, "trigger"));

        match trigger_index {
            None => {
                options = Some(ast.clone());
            }
            Some(ti) => {
                let value = &values[ti];
                match &value.kind {
                    ExprKind::PropertyRead { receiver, name, .. }
                        if matches!(receiver.kind, ExprKind::ImplicitReceiver) =>
                    {
                        reference = Some(name.clone());
                        // Build a LiteralMap with the `trigger` key/value removed.
                        let new_keys: Vec<LiteralMapKey> = keys
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i != ti)
                            .map(|(_, k)| k.clone())
                            .collect();
                        let new_values: Vec<AstNode> = values
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i != ti)
                            .map(|(_, v)| v.clone())
                            .collect();
                        options = Some(AstNode {
                            span: ast.span,
                            source_span: ast.source_span,
                            kind: ExprKind::LiteralMap {
                                keys: new_keys,
                                values: new_values,
                            },
                        });
                    }
                    _ => {
                        return Err(
                            "\"trigger\" option of the \"viewport\" trigger must be an identifier"
                                .to_string(),
                        );
                    }
                }
            }
        }
    }

    if is_hydration_trigger && reference.is_some() {
        return Err("\"viewport\" hydration trigger cannot have a \"trigger\"".to_string());
    } else if let Some(opts) = &options {
        if let Some(node) = find_dynamic_node(opts) {
            return Err(format!(
                "Options of the \"viewport\" trigger must be an object literal containing only \
                 literal values, but \"{}\" was found",
                dynamic_node_name(node)
            ));
        }
    }

    Ok(t::DeferredTrigger {
        kind: t::DeferredTriggerKind::Viewport { reference, options },
        spans,
    })
}

/// Whether a [`LiteralMapKey`] is a property key equal to `name`.
fn property_key_is(k: &LiteralMapKey, name: &str) -> bool {
    matches!(k, LiteralMapKey::Property { key, .. } if key == name)
}

// ---------------------------------------------------------------------------
// DynamicAstValidator — only ASTWithSource/LiteralPrimitive/LiteralArray/LiteralMap allowed.
// ---------------------------------------------------------------------------

/// `DynamicAstValidator.findDynamicNode` — returns the first node that is NOT one of the
/// allowed literal kinds (recursively). Our AST is already unwrapped from `ASTWithSource`,
/// so the root node is allowed to be a `LiteralMap`; we then recurse into array/map values.
fn find_dynamic_node(ast: &AstNode) -> Option<&AstNode> {
    match &ast.kind {
        ExprKind::LiteralPrimitive { .. } => None,
        ExprKind::LiteralArray { expressions } => {
            expressions.iter().find_map(find_dynamic_node)
        }
        ExprKind::LiteralMap { values, .. } => values.iter().find_map(find_dynamic_node),
        _ => Some(ast),
    }
}

/// The `constructor.name` reported for a dynamic node (a small, common subset).
fn dynamic_node_name(ast: &AstNode) -> &'static str {
    match &ast.kind {
        ExprKind::PropertyRead { .. } => "PropertyRead",
        ExprKind::SafePropertyRead { .. } => "SafePropertyRead",
        ExprKind::KeyedRead { .. } => "KeyedRead",
        ExprKind::SafeKeyedRead { .. } => "SafeKeyedRead",
        ExprKind::Binary { .. } => "Binary",
        ExprKind::Unary { .. } => "Unary",
        ExprKind::Conditional { .. } => "Conditional",
        ExprKind::BindingPipe { .. } => "BindingPipe",
        ExprKind::Call { .. } => "Call",
        ExprKind::SafeCall { .. } => "SafeCall",
        ExprKind::ImplicitReceiver => "ImplicitReceiver",
        ExprKind::ThisReceiver => "ThisReceiver",
        ExprKind::Interpolation { .. } => "Interpolation",
        ExprKind::Chain { .. } => "Chain",
        _ => "AST",
    }
}

// ---------------------------------------------------------------------------
// ParsedParameter + OnTriggerParser.
// ---------------------------------------------------------------------------

/// `ParsedParameter` — a single `on` trigger parameter (raw text + start offset).
struct ParsedParameter {
    /// Raw text of the parameter.
    expression: String,
    /// Index within the trigger expression at which the parameter starts.
    start: usize,
}

use crate::expression::lexer::{Lexer, Token, TokenType};

/// `OnTriggerParser` — the `on` micro-syntax parser (token stream walk + bracket stack).
struct OnTriggerParser<'a, 'e> {
    expression: &'a str,
    binding_parser: &'a ExprParser,
    start: usize,
    span: &'a MlSpan,
    triggers: &'a mut t::DeferredBlockTriggers,
    errors: &'e mut Vec<MlParseError>,
    validator: ReferenceTriggerValidator,
    is_hydration_trigger: bool,
    prefetch_span: Option<R3Span>,
    on_source_span: R3Span,
    hydrate_span: Option<R3Span>,
    index: usize,
    tokens: Vec<Token>,
}

impl<'a, 'e> OnTriggerParser<'a, 'e> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        expression: &'a str,
        binding_parser: &'a ExprParser,
        start: usize,
        span: &'a MlSpan,
        triggers: &'a mut t::DeferredBlockTriggers,
        errors: &'e mut Vec<MlParseError>,
        validator: ReferenceTriggerValidator,
        is_hydration_trigger: bool,
        prefetch_span: Option<R3Span>,
        on_source_span: R3Span,
        hydrate_span: Option<R3Span>,
    ) -> Self {
        let tokens = Lexer::new().tokenize(slice_from(expression, start));
        OnTriggerParser {
            expression,
            binding_parser,
            start,
            span,
            triggers,
            errors,
            validator,
            is_hydration_trigger,
            prefetch_span,
            on_source_span,
            hydrate_span,
            index: 0,
            tokens,
        }
    }

    fn parse(&mut self) {
        while !self.tokens.is_empty() && self.index < self.tokens.len() {
            let token = self.token().clone();

            if !token.is_identifier() {
                self.unexpected_token(&token);
                break;
            }

            if self.is_followed_by_or_last(CHAR_COMMA) {
                self.consume_trigger(&token, &[]);
                self.advance();
            } else if self.is_followed_by_or_last(CHAR_LPAREN) {
                self.advance(); // Advance to the opening paren.
                let prev_errors = self.errors.len();
                let parameters = self.consume_parameters();
                if self.errors.len() != prev_errors {
                    break;
                }
                self.consume_trigger(&token, &parameters);
                self.advance(); // Advance past the closing paren.
            } else if self.index < self.tokens.len() - 1 {
                let next = self.tokens[self.index + 1].clone();
                self.unexpected_token(&next);
            }

            self.advance();
        }
    }

    fn advance(&mut self) {
        self.index += 1;
    }

    fn is_followed_by_or_last(&self, ch: u32) -> bool {
        if self.index == self.tokens.len() - 1 {
            return true;
        }
        self.tokens[self.index + 1].is_character(ch)
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index.min(self.tokens.len() - 1)]
    }

    fn consume_trigger(&mut self, identifier: &Token, parameters: &[ParsedParameter]) {
        let first_index = self.tokens[0].index;
        let trigger_name_start_offset =
            (self.start as isize) + (identifier.index as isize) - (first_index as isize);
        let trigger_name_start = self.span.start.move_by(trigger_name_start_offset);

        let ident_str = identifier.to_string_value().unwrap_or_default();
        let name_span = r3_span_between(
            &trigger_name_start,
            &trigger_name_start.move_by(ident_str.chars().count() as isize),
        );
        let end_span =
            trigger_name_start.move_by((self.token().end - identifier.index) as isize);

        // Put the prefetch/on/hydrate spans with the first trigger only.
        let is_first_trigger = identifier.index == 0;
        let on_source_span = if is_first_trigger {
            Some(self.on_source_span.clone())
        } else {
            None
        };
        let prefetch_source_span = if is_first_trigger {
            self.prefetch_span.clone()
        } else {
            None
        };
        let hydrate_source_span = if is_first_trigger {
            self.hydrate_span.clone()
        } else {
            None
        };
        let source_span = R3Span {
            start: if is_first_trigger {
                self.span.start.offset as u32
            } else {
                trigger_name_start.offset as u32
            },
            end: end_span.offset as u32,
        };

        // Spans for the idle case ("first-trigger gets the keyword spans", null otherwise).
        let spans_first_aware = t::TriggerSpans {
            name_span: Some(name_span.clone()),
            source_span: source_span.clone(),
            prefetch_span: prefetch_source_span,
            when_or_on_source_span: on_source_span,
            hydrate_span: hydrate_source_span,
        };

        // Spans using the always-original `this.*` keyword spans (non-idle cases).
        let spans_always = t::TriggerSpans {
            name_span: Some(name_span),
            source_span,
            prefetch_span: self.prefetch_span.clone(),
            when_or_on_source_span: Some(self.on_source_span.clone()),
            hydrate_span: self.hydrate_span.clone(),
        };

        let ty = OnTriggerType::from_str(&ident_str);

        let result: TriggerResult<(t::TriggerKey, t::DeferredTrigger)> = match ty {
            Some(OnTriggerType::Idle) => {
                // idle uses the null-on-non-first spans (the documented asymmetry).
                create_idle_trigger(parameters, spans_first_aware)
                    .map(|tr| (t::TriggerKey::Idle, tr))
            }
            Some(OnTriggerType::Timer) => create_timer_trigger(parameters, spans_always.clone())
                .map(|tr| (t::TriggerKey::Timer, tr)),
            Some(OnTriggerType::Interaction) => {
                create_interaction_trigger(parameters, spans_always.clone(), self.validator)
                    .map(|tr| (t::TriggerKey::Interaction, tr))
            }
            Some(OnTriggerType::Immediate) => {
                create_immediate_trigger(parameters, spans_always.clone())
                    .map(|tr| (t::TriggerKey::Immediate, tr))
            }
            Some(OnTriggerType::Hover) => {
                create_hover_trigger(parameters, spans_always.clone(), self.validator)
                    .map(|tr| (t::TriggerKey::Hover, tr))
            }
            Some(OnTriggerType::Viewport) => {
                let binding_base = self.span.start.offset as i32;
                create_viewport_trigger(
                    self.start,
                    self.is_hydration_trigger,
                    self.binding_parser,
                    parameters,
                    spans_always.clone(),
                    to_r3_span(self.span),
                    binding_base,
                    self.validator,
                )
                .map(|tr| (t::TriggerKey::Viewport, tr))
            }
            Some(OnTriggerType::Never) | None => {
                Err(format!("Unrecognized trigger type \"{}\"", ident_str))
            }
        };

        match result {
            Ok((key, trigger)) => {
                track_trigger(key, self.triggers, self.errors, trigger);
            }
            Err(msg) => self.error(identifier, &msg),
        }
    }

    fn consume_parameters(&mut self) -> Vec<ParsedParameter> {
        let mut parameters: Vec<ParsedParameter> = Vec::new();

        if !self.token().is_character(CHAR_LPAREN) {
            let tok = self.token().clone();
            self.unexpected_token(&tok);
            return parameters;
        }

        self.advance();

        let mut comma_delim_stack: Vec<u32> = Vec::new();
        let mut tokens: Vec<Token> = Vec::new();

        while self.index < self.tokens.len() {
            let token = self.token().clone();

            // Stop when we hit the closing `)` outside any comma-delimited syntax.
            if token.is_character(CHAR_RPAREN) && comma_delim_stack.is_empty() {
                if !tokens.is_empty() {
                    parameters.push(ParsedParameter {
                        expression: self.token_range_text(&tokens),
                        start: tokens[0].index as usize,
                    });
                }
                break;
            }

            // Opening bracket of a comma-delimited syntax.
            if token.token_type() == TokenType::Character {
                if let Some(closing) = comma_delimited_closing(token_char_code(&token)) {
                    comma_delim_stack.push(closing);
                }
            }

            if let Some(&top) = comma_delim_stack.last() {
                if token.is_character(top) {
                    comma_delim_stack.pop();
                }
            }

            // Top-level comma → new parameter.
            if comma_delim_stack.is_empty()
                && token.is_character(CHAR_COMMA)
                && !tokens.is_empty()
            {
                parameters.push(ParsedParameter {
                    expression: self.token_range_text(&tokens),
                    start: tokens[0].index as usize,
                });
                self.advance();
                tokens = Vec::new();
                continue;
            }

            tokens.push(token);
            self.advance();
        }

        if !self.token().is_character(CHAR_RPAREN) || !comma_delim_stack.is_empty() {
            let tok = self.token().clone();
            self.error(&tok, "Unexpected end of expression");
        }

        if self.index < self.tokens.len() - 1
            && !self.tokens[self.index + 1].is_character(CHAR_COMMA)
        {
            let next = self.tokens[self.index + 1].clone();
            self.unexpected_token(&next);
        }

        parameters
    }

    fn token_range_text(&self, tokens: &[Token]) -> String {
        if tokens.is_empty() {
            return String::new();
        }
        let from = self.start + tokens[0].index as usize;
        let to = self.start + tokens[tokens.len() - 1].end as usize;
        slice_range(self.expression, from, to).to_string()
    }

    fn error(&mut self, token: &Token, message: &str) {
        let new_start = self.span.start.move_by((self.start + token.index as usize) as isize);
        let new_end = new_start.move_by((token.end - token.index) as isize);
        self.errors.push(MlParseError::new(
            MlSpan::new(new_start, new_end),
            message.to_string(),
        ));
    }

    fn unexpected_token(&mut self, token: &Token) {
        let s = token.to_string_value().unwrap_or_default();
        self.error(token, &format!("Unexpected token \"{}\"", s));
    }
}

/// The closing char of a comma-delimited syntax pair, or `None`.
fn comma_delimited_closing(open: u32) -> Option<u32> {
    match open {
        CHAR_LBRACE => Some(CHAR_RBRACE),
        CHAR_LBRACKET => Some(CHAR_RBRACKET),
        CHAR_LPAREN => Some(CHAR_RPAREN),
        _ => None,
    }
}

/// The char code carried by a `Character` token (0 otherwise).
fn token_char_code(token: &Token) -> u32 {
    use crate::expression::lexer::TokenValue;
    match &token.kind {
        TokenValue::Character(c) => *c,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// getTriggerParametersStart / parseDeferredTime.
// ---------------------------------------------------------------------------

/// `getTriggerParametersStart(value, startPosition)` — char index of the first
/// non-whitespace char *after* a whitespace separator, or `value.len()` semantics:
/// returns the char index, or `usize::MAX` for the TS `-1` (no params). Callers slice
/// with [`slice_from`], which treats `usize::MAX`/out-of-range as "empty".
pub fn get_trigger_parameters_start(value: &str, start_position: usize) -> usize {
    let chars: Vec<char> = value.chars().collect();
    let mut has_found_separator = false;
    let mut i = start_position;
    while i < chars.len() {
        if is_separator_char(chars[i]) {
            has_found_separator = true;
        } else if has_found_separator {
            return i;
        }
        i += 1;
    }
    // TS returns -1; we use usize::MAX as the sentinel.
    usize::MAX
}

/// `parseDeferredTime(value)` — parse a time literal to milliseconds, or `None`.
/// Pattern: `^\d+\.?\d*(ms|s)?$`. Bare number and `ms` => ×1; `s` => ×1000.
pub fn parse_deferred_time(value: &str) -> Option<f64> {
    let (number_part, units): (&str, &str) = if let Some(stripped) = value.strip_suffix("ms") {
        (stripped, "ms")
    } else if let Some(stripped) = value.strip_suffix('s') {
        (stripped, "s")
    } else {
        (value, "")
    };

    if !matches_time_number(number_part) {
        return None;
    }

    let time: f64 = number_part.parse().ok()?;
    Some(time * if units == "s" { 1000.0 } else { 1.0 })
}

/// Matches the numeric part `\d+\.?\d*` (one-or-more digits, optional dot, zero-or-more
/// digits). Rejects a leading dot; allows a trailing dot.
fn matches_time_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() {
        return false;
    }
    let mut i = 0;
    // `\d+`
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    // `\.?`
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
    }
    // `\d*`
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i == bytes.len()
}

// ---------------------------------------------------------------------------
// Small string / AST helpers.
// ---------------------------------------------------------------------------

/// Char-based `String.indexOf(needle)` returning a char index.
fn char_index_of(haystack: &str, needle: &str) -> Option<usize> {
    let byte_idx = haystack.find(needle)?;
    Some(haystack[..byte_idx].chars().count())
}

/// `value.slice(start)` with char semantics; `start == usize::MAX` (the `-1` sentinel)
/// or out-of-range yields the empty string.
fn slice_from(value: &str, start: usize) -> &str {
    if start == usize::MAX {
        return "";
    }
    let mut indices = value.char_indices();
    match indices.nth(start) {
        Some((byte_idx, _)) => &value[byte_idx..],
        None => "",
    }
}

/// `value.slice(from, to)` with char semantics.
fn slice_range(value: &str, from: usize, to: usize) -> &str {
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    let start_byte = chars.get(from).map(|(b, _)| *b).unwrap_or(value.len());
    let end_byte = chars.get(to).map(|(b, _)| *b).unwrap_or(value.len());
    if start_byte > end_byte {
        return "";
    }
    &value[start_byte..end_byte]
}

/// Unwrap an [`AstWithSource`] to its inner [`AstNode`] (TS `parsed.ast`).
fn aws_into_ast(parsed: AstWithSource) -> AstNode {
    *parsed.ast
}

/// Build a [`MlSpan`]-shaped error span when only offset-only r3 offsets are available.
/// NOTE(port): the connected `parse_util` port should expose a way to construct a span
/// from raw offsets + a file handle; until then duplicate-trigger errors carry the
/// offsets in a degenerate span anchored at the trigger's source offsets via the global
/// file-less location helper below.
fn offset_only_ml_span(span: &R3Span) -> MlSpan {
    use crate::ml_parser::{ParseLocation, ParseSourceFile};
    use std::rc::Rc;
    // NOTE(port): no file content is available at this layer, so we synthesize a
    // file-less location carrying only the offsets. Line/col are 0; downstream code that
    // needs them re-derives from `offset`. This preserves the offset span used by the
    // language service while avoiding a dependency on the originating file.
    let file = Rc::new(ParseSourceFile {
        content: String::new(),
        url: String::new(),
    });
    let start = ParseLocation::new(file.clone(), span.start as usize, 0, 0);
    let end = ParseLocation::new(file, span.end as usize, 0, 0);
    MlSpan::new(start, end)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml_parser::{parse as html_parse, Node as MlNode};

    /// Parse a template and return its first root block (the `@defer`) plus the
    /// connected blocks that follow it as siblings.
    fn defer_and_connected(src: &str) -> (HtmlBlock, Vec<HtmlBlock>) {
        let result = html_parse(src, "test.html");
        assert!(
            result.errors.is_empty(),
            "html parse errors: {:?}",
            result.errors
        );
        let mut blocks: Vec<HtmlBlock> = result
            .root_nodes
            .into_iter()
            .filter_map(|n| match n {
                MlNode::Block(b) => Some(*b),
                _ => None,
            })
            .collect();
        assert!(!blocks.is_empty(), "no blocks parsed");
        let defer = blocks.remove(0);
        (defer, blocks)
    }

    fn no_transform() -> impl FnMut(&[MlNode]) -> Vec<t::Node> {
        |_nodes: &[MlNode]| Vec::new()
    }

    #[test]
    fn parses_defer_on_idle_with_placeholder() {
        let src = "@defer (on idle) {<a></a>} @placeholder {<b></b>}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();

        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);

        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );

        // The `idle` trigger is present in the regular trigger set.
        let idle = result
            .node
            .triggers
            .idle
            .as_ref()
            .expect("expected an idle trigger");
        assert_eq!(idle.kind, t::DeferredTriggerKind::Idle { timeout: None });
        assert_eq!(result.node.triggers.order, vec![t::TriggerKey::Idle]);

        // `on` keyword span is attached (first trigger).
        assert!(idle.spans.when_or_on_source_span.is_some());
        assert!(idle.spans.prefetch_span.is_none());
        assert!(idle.spans.hydrate_span.is_none());

        // The @placeholder connected block is captured.
        assert!(result.node.placeholder.is_some());
        assert!(result.node.loading.is_none());
        assert!(result.node.error.is_none());
    }

    #[test]
    fn parses_timer_with_milliseconds() {
        let src = "@defer (on timer(500ms)) {x}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();
        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let timer = result.node.triggers.timer.as_ref().expect("timer");
        assert_eq!(timer.kind, t::DeferredTriggerKind::Timer { delay: 500.0 });
    }

    #[test]
    fn parses_prefetch_on_hover_into_prefetch_set() {
        let src = "@defer (prefetch on hover) {x}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();
        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.node.triggers.hover.is_none());
        let hover = result.node.prefetch_triggers.hover.as_ref().expect("hover");
        assert_eq!(
            hover.kind,
            t::DeferredTriggerKind::Hover { reference: None }
        );
        // prefetch span attached on the first trigger.
        assert!(hover.spans.prefetch_span.is_some());
    }

    #[test]
    fn parses_hydrate_never() {
        let src = "@defer (hydrate never) {x}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();
        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let never = result.node.hydrate_triggers.never.as_ref().expect("never");
        assert_eq!(never.kind, t::DeferredTriggerKind::Never);
        assert!(never.spans.hydrate_span.is_some());
        assert!(never.spans.when_or_on_source_span.is_none());
    }

    #[test]
    fn duplicate_trigger_is_an_error() {
        let src = "@defer (on idle; on idle) {x}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();
        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.msg.contains("Duplicate \"idle\" trigger")),
            "errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn unrecognized_trigger_is_an_error() {
        let src = "@defer (bananas) {x}";
        let (defer, connected) = defer_and_connected(src);
        let parser = ExprParser::default();
        let mut transform = no_transform();
        let result = create_deferred_block(&defer, &connected, &mut transform, &parser);
        assert!(
            result.errors.iter().any(|e| e.msg == "Unrecognized trigger"),
            "errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn parse_deferred_time_units() {
        assert_eq!(parse_deferred_time("500"), Some(500.0));
        assert_eq!(parse_deferred_time("500ms"), Some(500.0));
        assert_eq!(parse_deferred_time("2s"), Some(2000.0));
        assert_eq!(parse_deferred_time("5."), Some(5.0));
        assert_eq!(parse_deferred_time(".5"), None);
        assert_eq!(parse_deferred_time("abc"), None);
        assert_eq!(parse_deferred_time("1.5s"), Some(1500.0));
    }

    #[test]
    fn get_trigger_parameters_start_basic() {
        // "on idle" -> after "on" + space, params start at index 3.
        assert_eq!(get_trigger_parameters_start("on idle", 0), 3);
        // No separator after start.
        assert_eq!(get_trigger_parameters_start("idle", 0), usize::MAX);
    }

    #[test]
    fn is_connected_block_predicate() {
        assert!(is_connected_defer_loop_block("placeholder"));
        assert!(is_connected_defer_loop_block("loading"));
        assert!(is_connected_defer_loop_block("error"));
        assert!(!is_connected_defer_loop_block("defer"));
    }
}
