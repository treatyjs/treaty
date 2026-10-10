//! Per-kind adapters: ng-analyze's `ClassMetadata` -> plain [`DecoratorFacts`].
//!
//! ng-analyze reports spans in UTF-16 and several values only as spans. Every adapter reads the
//! source text through [`Ctx::text`], which converts the span to bytes and slices the bytes, so
//! the facts carry JavaScript source text, never an oxc node.

use ng_analyze::types::metadata::{
    ClassMetadata, ExpressionValueMetadata, ReferenceMetadata, SpanMetadata,
};
use ng_analyze::AnalysisResult;

use crate::analyze::analyze_source;
use treaty_ivy_facts::facts::{ClassKind, DecoratorFacts, ExprFact};
use treaty_ivy_facts::span::{Span, Utf16Index};

mod directive;
mod others;

/// Facts for one analysed file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileFacts {
    /// One entry per decorated class ng-analyze reported, in source order.
    pub classes: Vec<DecoratorFacts>,
    /// File-level failures: the analyser refused the file, or a diagnostic lies outside every class.
    /// When non-empty the consumer must fall back for every class in the file.
    pub file_errors: Vec<String>,
}

/// Analyses `source` with ng-analyze and adapts the result.
pub fn extract(file_name: &str, source: &str, resources: &[(String, String)]) -> FileFacts {
    match analyze_source(file_name, source, resources) {
        Ok(result) => adapt_result(source, &result),
        Err(e) => FileFacts { classes: Vec::new(), file_errors: vec![e] },
    }
}

/// Adapts an ng-analyze result (exposed for the adapter unit tests).
pub fn adapt_result(source: &str, result: &AnalysisResult) -> FileFacts {
    let ctx = Ctx { source, idx: Utf16Index::new(source) };
    let mut file = FileFacts::default();

    // Error diagnostics (category 1 = error in oxc/TypeScript terms). Non-errors are not fatal.
    let mut errors: Vec<(Option<Span>, String)> = Vec::new();
    for d in &result.diagnostics {
        if d.category != 1 {
            continue;
        }
        let span = d.span.as_ref().and_then(|s| ctx.span(s));
        errors.push((span, format!("ng-analyze {}: {}", d.code, d.message_text)));
    }

    let mut spans: Vec<Span> = Vec::new();
    for class in &result.classes {
        let Some(class_span) = ctx.span(&class.span) else {
            file.file_errors.push("ng-analyze class span out of range".to_string());
            continue;
        };
        spans.push(class_span);
        let Some(facts) = adapt_class(&ctx, class, class_span, &errors) else { continue };
        file.classes.push(facts);
    }
    for (span, msg) in errors {
        let inside = span.is_some_and(|s| spans.iter().any(|c| c.contains(s)));
        if !inside {
            file.file_errors.push(msg);
        }
    }
    file
}

/// Shared read-only context for the adapters.
pub(crate) struct Ctx<'a> {
    pub source: &'a str,
    pub idx: Utf16Index,
}

impl Ctx<'_> {
    /// UTF-16 span to byte span.
    pub fn span(&self, s: &SpanMetadata) -> Option<Span> {
        self.idx.span(s.start, s.end)
    }

    /// The source text under a UTF-16 span.
    pub fn slice(&self, s: &SpanMetadata) -> Option<String> {
        let span = self.span(s)?;
        self.source.get(span.start..span.end).map(str::to_string)
    }

    /// The text of an expression value: ng-analyze's folded text when it has one (identifiers, string
    /// literals without their quotes), otherwise the source text under its span.
    pub fn text(&self, v: &ExpressionValueMetadata) -> Option<String> {
        v.text.clone().or_else(|| self.slice(&v.source_span))
    }

    /// The source text of a span-only value (a provider array, a transform, ...).
    pub fn expr(&self, s: Option<&SpanMetadata>) -> Option<ExprFact> {
        self.slice(s?).map(ExprFact::new)
    }
}

/// The name a reference resolves to in the consumer: the local alias, else the imported symbol.
pub(crate) fn reference_name(r: &ReferenceMetadata) -> Option<String> {
    r.local_alias
        .clone()
        .or_else(|| r.consumer_import.as_ref().map(|i| i.symbol.clone()))
        .or_else(|| r.typecheck_import.as_ref().map(|i| i.symbol.clone()))
}

fn adapt_class(
    ctx: &Ctx<'_>,
    class: &ClassMetadata,
    class_span: Span,
    errors: &[(Option<Span>, String)],
) -> Option<DecoratorFacts> {
    let kind = if class.component.is_some() {
        Some(ClassKind::Component)
    } else if class.directive.is_some() {
        Some(ClassKind::Directive)
    } else if class.pipe.is_some() {
        Some(ClassKind::Pipe)
    } else if class.injectable.is_some() {
        Some(ClassKind::Injectable)
    } else if class.ng_module.is_some() {
        Some(ClassKind::NgModule)
    } else {
        // `@Service` and other decorators Treaty does not recognise: Treaty skips such classes, so
        // they are not facts. ng-analyze reporting a `service` is noted as unsupported only if it
        // carries an error.
        None
    };
    let kind = kind?;

    let mut facts = DecoratorFacts {
        class_name: class.class_name.clone(),
        class_start: class_span.start,
        kind: Some(kind),
        ..Default::default()
    };
    for (span, msg) in errors {
        if span.is_some_and(|s| class_span.contains(s)) {
            facts.unsupported.push(msg.clone());
        }
    }
    if !facts.unsupported.is_empty() {
        return Some(facts);
    }

    // Treaty compiles every recognised decorator on a class (e.g. `@Pipe` + `@Injectable`), so every
    // section ng-analyze reports is adapted, not only the first.
    if class.component.is_some() || class.directive.is_some() {
        directive::adapt(ctx, class, &mut facts);
    }
    others::adapt(ctx, class, &mut facts);
    facts.constructor_deps = others::constructor_deps(ctx, class);
    Some(facts)
}
