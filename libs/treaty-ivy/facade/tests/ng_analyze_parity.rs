//! Parity between Treaty's own decorator facts and ng-analyze's, over Angular's compliance corpus.
//!
//! Feature-gated (`--features ng-analyze`). For every `.ts` file under the corpus, both extractors
//! produce [`DecoratorFacts`] for each decorated class. The facts are normalised (expressions lowered
//! through Treaty's own converter, queries and member lists sorted, `changeDetection` reduced to its
//! semantic value) and diffed field by field. A difference fails the test unless [`classify`] gives it
//! a written reason.
//!
//! Set `NG_ANALYZE_PARITY_REPORT=<abs path>` to write the JSON report. It is skipped with a message
//! when the corpus is absent.
#![cfg(feature = "ng-analyze")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use treaty_ivy::source_compile::{decorator_facts_of_source, lower_to_text};
use treaty_ivy_ng_bridge::facts::{
    ComponentFacts, DecoratorFacts, DirectiveFacts, HostValue, InjectableFacts, NgModuleFacts,
    PipeFacts, QueryFact, QueryPredicateFact,
};

/// Expected differences, each with the reason it is expected. A difference is accepted only when
/// [`classify`] returns a reason for it; every other difference fails the test. Reasons say which
/// extractor is right and why, and entries marked `Treaty defect` are cases where ng-analyze is right
/// and Treaty's extractor is wrong. Those are kept visible here, not hidden by changing Treaty's
/// extractor in this task.
/// Why Treaty's refusal of a class is expected while the bridge extracts it. `None` when unexplained.
fn explain_refusal(_reason: &str, _source: &str, _b: &DecoratorFacts) -> Option<&'static str> {
    // Every refusal the bridge extracts is now extracted by Treaty too (same-file constants, expression
    // `host` values and qualifier-first `deps` are read). A new refusal is an unexplained difference.
    None
}

fn classify(field: &str, rel: &str, source: &str, tv: &str, bv: &str) -> Option<&'static str> {
    let _ = rel;
    match field {
        // ng-analyze substitutes the class name for an absent pipe `name` (its `read_optional_string`
        // fallback). ngtsc reports an error for a nameless pipe, so Treaty's None is the correct fact.
        // `imports: [provideX()]` where `provideX(): ModuleWithProviders<M>`: ngtsc resolves the call to
        // the module M. Treaty resolves these in `resolve_module_with_providers_imports` during emit, not
        // in its decorator facts, so the raw facts list no name for the call.
 "ng_module.imports" if tv == "Some([])" && source.contains("ModuleWithProviders") => Some(
            "bridge resolves a ModuleWithProviders call to its module (ngtsc resolveModuleWithProviders); Treaty resolves it at emit, not in its facts",
        ),
        // ng-analyze substitutes the class name for an absent pipe `name` (its `read_optional_string`
        // fallback). ngtsc reports an error for a nameless pipe, so Treaty's None is the correct fact.
        // `input.required({transform: fn})` / `input({transform})`: ng-analyze reports the transform, but
        // Angular's compiled definition does not capture a signal input's transform (golden
        // `signal_inputs/should not capture the transform function in the compiled JS for signal inputs`).
        // Treaty's None is right, and the facts path drops it too.
        "directive.inputs.transform" if source.contains("transform:") && tv.ends_with("=None") && !bv.ends_with("=None") => Some(
            "bridge over-reports: ng-analyze gives a signal input's transform; Angular's definition omits it (golden signal_inputs), so Treaty's None is right",
        ),
        // ng-analyze substitutes the class name for an absent pipe `name` (its `read_optional_string`
        // fallback). ngtsc reports an error for a nameless pipe, so Treaty's None is the correct fact.
        "pipe.name" if tv == "None" => Some(
            "bridge limitation: ng-analyze defaults an absent pipe name to the class name; ngtsc errors on it",
        ),
        _ => None,
    }
}

#[derive(Default)]
struct Report {
    files: usize,
    files_with_classes: usize,
    classes_compared: usize,
    classes_both_extracted: usize,
    /// Bridge refused the class: the emit path would fall back to Treaty's extractor. Grouped by reason.
    fallback_by_reason: BTreeMap<String, (usize, String)>,
    /// Treaty refused the class and the bridge did not: Treaty's extractor cannot handle it.
    treaty_refused_bridge_extracted: BTreeMap<String, (usize, String)>,
    /// Treaty refusals the bridge extracts, each explained by a written reason.
    refusals_explained: BTreeMap<String, (usize, String)>,
    /// Field differences accepted by [`classify`], keyed by the written reason.
    explained: BTreeMap<String, (usize, String)>,
    /// Classes ng-analyze reports that Treaty does not (Treaty reads top-level and exported classes).
    bridge_only_classes: BTreeMap<String, (usize, String)>,
    /// Field-level differences, grouped by field path.
    diffs: BTreeMap<String, (usize, String)>,
    /// Field differences that are not allowlisted (test failures).
    unexpected: BTreeMap<String, (usize, String)>,
    file_errors: Vec<String>,
    /// Classes both extractors refuse (both fall back to the same behaviour).
    classes_both_refused: usize,
}

fn bump(map: &mut BTreeMap<String, (usize, String)>, key: &str, example: impl FnOnce() -> String) {
    let e = map.entry(key.to_string()).or_insert_with(|| (0, example()));
    e.0 += 1;
}

fn corpus_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = manifest
        .join("..")
        .join("..")
        .join("..")
        .join("tools/angular-ref/packages/compiler-cli/test/compliance/test_cases");
    p.canonicalize().unwrap_or(p)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("ts")
            && !p.to_string_lossy().ends_with(".d.ts")
        {
            out.push(p);
        }
    }
}

/// Sibling files of `source` (same directory), keyed by their path relative to the corpus root, so a
/// `templateUrl` or a cross-file import resolves the same way for the bridge as it would on disk.
fn siblings(root: &Path, source: &Path) -> Vec<(String, String)> {
    let Some(dir) = source.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() || p == source {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&p) else { continue };
        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        out.push((rel, content));
    }
    out
}

fn expr(text: &str) -> String {
    lower_to_text(text).unwrap_or_else(|| format!("<unlowerable {text}>"))
}

fn expr_opt(e: &Option<treaty_ivy_ng_bridge::facts::ExprFact>) -> Option<String> {
    e.as_ref().map(|x| expr(&x.text))
}

/// `providers` as compared: an empty array and an absent option are the same (ngtsc omits an empty
/// `providers` array from the definition, and ng-analyze reports it as absent).
fn providers_opt(e: &Option<treaty_ivy_ng_bridge::facts::ExprFact>) -> Option<String> {
    expr_opt(e).filter(|s| s != "[]")
}

fn query_key(q: &QueryFact) -> (String, bool) {
    (q.property.clone(), q.is_content)
}

/// Selector locators are split on commas and trimmed, as ngtsc's `query_generation.ts` does for every
/// predicate string (decorator and signal alike), so the two extractors compare on the same list.
fn split_selectors(list: &[String]) -> Vec<String> {
    list.iter().flat_map(|s| s.split(',').map(|p| p.trim().to_string())).collect()
}

/// Classes keyed by (class name, occurrence index of that name in the file).
fn keyed(list: &[DecoratorFacts]) -> BTreeMap<(String, usize), &DecoratorFacts> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for c in list {
        let name = c.class_name.clone().unwrap_or_default();
        let n = seen.entry(name.clone()).or_insert(0);
        *n += 1;
        out.insert((name, *n), c);
    }
    out
}

fn norm_query(q: &QueryFact) -> String {
    let predicate = match &q.predicate {
        QueryPredicateFact::Selectors(s) => format!("selectors{:?}", split_selectors(s)),
        QueryPredicateFact::Expr(e) => format!("expr {}", expr(&e.text)),
    };
    format!(
        "{}|first={}|content={}|pred={}|desc={}|read={:?}|static={}|signal={}",
        q.property,
        q.first,
        q.is_content,
        predicate,
        q.descendants,
        q.read.as_ref().map(|r| expr(&r.text)),
        q.is_static,
        q.is_signal
    )
}

/// `changeDetection` reduced to its semantic value: only `Default` (or its `Eager` alias) differs
/// from Angular's OnPush default, so an explicit `OnPush` and an absent option are the same.
fn norm_change_detection(v: &Option<String>) -> Option<String> {
    match v.as_deref() {
        Some("Default") | Some("Eager") | Some("1") => Some("Default".to_string()),
        _ => None,
    }
}

fn host_value(v: &HostValue) -> String {
    match v {
        HostValue::Str(s) => format!("str {s:?}"),
        HostValue::Expr(e) => format!("expr {}", expr(e)),
    }
}

/// Diffs one class. Returns (field path, treaty value, bridge value) for every mismatch.
fn diff_class(t: &DecoratorFacts, b: &DecoratorFacts) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut push = |field: &str, tv: String, bv: String| {
        if tv != bv {
            out.push((field.to_string(), tv, bv));
        }
    };
    // Sections are compared by presence; the `kind` label depends on decorator order, which the two
    // extractors do not record the same way for multi-decorator classes.
    diff_directive(t.directive.as_ref(), b.directive.as_ref(), &mut push);
    diff_component(t.component.as_ref(), b.component.as_ref(), &mut push);
    diff_pipe(t.pipe.as_ref(), b.pipe.as_ref(), &mut push);
    diff_injectable(t.injectable.as_ref(), b.injectable.as_ref(), &mut push);
    diff_ng_module(t.ng_module.as_ref(), b.ng_module.as_ref(), &mut push);
    push(
        "constructor_deps",
        format!("{:?}", t.constructor_deps.as_ref().map(|v| v.iter().map(dep_repr).collect::<Vec<_>>())),
        format!("{:?}", b.constructor_deps.as_ref().map(|v| v.iter().map(dep_repr).collect::<Vec<_>>())),
    );
    out
}

fn dep_repr(d: &treaty_ivy_ng_bridge::facts::DepFact) -> String {
    format!(
        "token={:?} attr={:?} opt={} host={} self={} skipSelf={}",
        expr_opt(&d.token),
        d.attribute,
        d.optional,
        d.host,
        d.self_,
        d.skip_self
    )
}

fn diff_directive(
    t: Option<&DirectiveFacts>,
    b: Option<&DirectiveFacts>,
    push: &mut impl FnMut(&str, String, String),
) {
    let (Some(t), Some(b)) = (t, b) else {
        push("directive", format!("{}", t.is_some()), format!("{}", b.is_some()));
        return;
    };
    push("directive.selector", format!("{:?}", t.selector), format!("{:?}", b.selector));
    push("directive.standalone", format!("{}", t.standalone), format!("{}", b.standalone));
    push("directive.signals", format!("{}", t.signals), format!("{}", b.signals));
    push("directive.export_as", format!("{:?}", t.export_as), format!("{:?}", b.export_as));
    let host = |h: &Option<Vec<(String, HostValue)>>| {
        h.as_ref().map(|v| v.iter().map(|(k, x)| format!("{k}={}", host_value(x))).collect::<Vec<_>>())
    };
    push("directive.host_object", format!("{:?}", host(&t.host_object)), format!("{:?}", host(&b.host_object)));
    let mut tb: Vec<String> = t.host_bindings.iter().map(|x| format!("{}<-{}", x.host_prop, x.member)).collect();
    let mut bb: Vec<String> = b.host_bindings.iter().map(|x| format!("{}<-{}", x.host_prop, x.member)).collect();
    tb.sort();
    bb.sort();
    push("directive.host_bindings", format!("{tb:?}"), format!("{bb:?}"));
    let mut tl: Vec<String> = t.host_listeners.iter().map(|x| format!("{}:{}({:?})", x.event, x.method, x.args)).collect();
    let mut bl: Vec<String> = b.host_listeners.iter().map(|x| format!("{}:{}({:?})", x.event, x.method, x.args)).collect();
    tl.sort();
    bl.sort();
    push("directive.host_listeners", format!("{tl:?}"), format!("{bl:?}"));
    let hd = |v: &Option<Vec<treaty_ivy_ng_bridge::facts::HostDirectiveFact>>| {
        v.as_ref().map(|list| {
            list.iter()
                .map(|h| format!("{}{} in={:?} out={:?}", h.name, if h.forward_ref { "(fwd)" } else { "" }, h.inputs, h.outputs))
                .collect::<Vec<_>>()
        })
    };
    push("directive.host_directives", format!("{:?}", hd(&t.host_directives)), format!("{:?}", hd(&b.host_directives)));
    push("directive.providers", format!("{:?}", expr_opt(&t.providers)), format!("{:?}", expr_opt(&b.providers)));
    diff_inputs(&t.inputs, &b.inputs, push);
    diff_outputs(&t.outputs, &b.outputs, push);
    let mut tq: Vec<String> = t.content_queries.iter().chain(t.view_queries.iter()).map(norm_query).collect();
    let mut bq: Vec<String> = b.content_queries.iter().chain(b.view_queries.iter()).map(norm_query).collect();
    tq.sort();
    bq.sort();
    push("directive.queries", format!("{tq:?}"), format!("{bq:?}"));
    let mut tcq: Vec<(String, bool)> = t.content_queries.iter().map(query_key).collect();
    let mut bcq: Vec<(String, bool)> = b.content_queries.iter().map(query_key).collect();
    tcq.sort();
    bcq.sort();
    push("directive.query_list_membership", format!("{tcq:?}"), format!("{bcq:?}"));
    push("directive.uses_on_changes", format!("{}", t.uses_on_changes), format!("{}", b.uses_on_changes));
}

/// Inputs compared per class property, so each mismatch names the property and the attribute.
fn diff_inputs(t: &[treaty_ivy_ng_bridge::facts::InputFact], b: &[treaty_ivy_ng_bridge::facts::InputFact], push: &mut impl FnMut(&str, String, String)) {
    let (ti, bi) = (input_index(t), input_index(b));
    let props: std::collections::BTreeSet<&String> = ti.keys().chain(bi.keys()).collect();
    for p in props {
        match (ti.get(p), bi.get(p)) {
            (Some(x), Some(y)) => {
                push("directive.inputs.public_name", format!("{p}={}", x.public_name), format!("{p}={}", y.public_name));
                push("directive.inputs.required", format!("{p}={}", x.required), format!("{p}={}", y.required));
                push("directive.inputs.is_signal", format!("{p}={}", x.is_signal), format!("{p}={}", y.is_signal));
                push("directive.inputs.from_decorator", format!("{p}={}", x.from_decorator), format!("{p}={}", y.from_decorator));
                push("directive.inputs.transform", format!("{p}={:?}", expr_opt(&x.transform)), format!("{p}={:?}", expr_opt(&y.transform)));
            }
            (x, y) => push("directive.inputs.presence", format!("{p} present={}", x.is_some()), format!("{p} present={}", y.is_some())),
        }
    }
}

fn input_index(v: &[treaty_ivy_ng_bridge::facts::InputFact]) -> BTreeMap<String, &treaty_ivy_ng_bridge::facts::InputFact> {
    v.iter().map(|i| (i.property.clone(), i)).collect()
}

fn output_index(v: &[treaty_ivy_ng_bridge::facts::OutputFact]) -> BTreeMap<String, &treaty_ivy_ng_bridge::facts::OutputFact> {
    v.iter().map(|o| (o.property.clone(), o)).collect()
}

/// Outputs compared per class property.
fn diff_outputs(t: &[treaty_ivy_ng_bridge::facts::OutputFact], b: &[treaty_ivy_ng_bridge::facts::OutputFact], push: &mut impl FnMut(&str, String, String)) {
    let (to, bo) = (output_index(t), output_index(b));
    let props: std::collections::BTreeSet<&String> = to.keys().chain(bo.keys()).collect();
    for p in props {
        match (to.get(p), bo.get(p)) {
            (Some(x), Some(y)) => push("directive.outputs.public_name", format!("{p}={}", x.public_name), format!("{p}={}", y.public_name)),
            (x, y) => push("directive.outputs.presence", format!("{p} present={}", x.is_some()), format!("{p} present={}", y.is_some())),
        }
    }
}

/// The `signals` flag Treaty EMITS (explicit option OR any signal input/query) against ngtsc's, which
/// is the explicit option only (`directive/src/shared.ts`, `isSignal`). The raw facts agree; this
/// compares what each extractor makes of them.
fn diff_component(
    t: Option<&ComponentFacts>,
    b: Option<&ComponentFacts>,
    push: &mut impl FnMut(&str, String, String),
) {
    let (Some(t), Some(b)) = (t, b) else {
        push("component", format!("{}", t.is_some()), format!("{}", b.is_some()));
        return;
    };
    push("component.template", format!("{:?}", t.template), format!("{:?}", b.template));
    push("component.template_url", format!("{:?}", t.template_url), format!("{:?}", b.template_url));
    push("component.styles", format!("{:?}", t.styles), format!("{:?}", b.styles));
    push("component.style_urls", format!("{:?}", t.style_urls), format!("{:?}", b.style_urls));
    push(
        "component.change_detection",
        format!("{:?}", norm_change_detection(&t.change_detection)),
        format!("{:?}", norm_change_detection(&b.change_detection)),
    );
    push("component.encapsulation", format!("{:?}", t.encapsulation), format!("{:?}", b.encapsulation));
    push("component.preserve_whitespaces", format!("{:?}", t.preserve_whitespaces), format!("{:?}", b.preserve_whitespaces));
    push("component.animations", format!("{:?}", expr_opt(&t.animations)), format!("{:?}", expr_opt(&b.animations)));
    push("component.view_providers", format!("{:?}", expr_opt(&t.view_providers)), format!("{:?}", expr_opt(&b.view_providers)));
    push("component.imports", format!("{:?}", t.imports), format!("{:?}", b.imports));
}

fn diff_pipe(t: Option<&PipeFacts>, b: Option<&PipeFacts>, push: &mut impl FnMut(&str, String, String)) {
    match (t, b) {
        (None, None) => {}
        (Some(t), Some(b)) => {
            push("pipe.name", format!("{:?}", t.name), format!("{:?}", b.name));
            push("pipe.pure", format!("{:?}", t.pure), format!("{:?}", b.pure));
            push("pipe.standalone", format!("{:?}", t.standalone), format!("{:?}", b.standalone));
        }
        _ => push("pipe", format!("{}", t.is_some()), format!("{}", b.is_some())),
    }
}

fn diff_injectable(
    t: Option<&InjectableFacts>,
    b: Option<&InjectableFacts>,
    push: &mut impl FnMut(&str, String, String),
) {
    match (t, b) {
        (None, None) => {}
        (Some(t), Some(b)) => {
            push("injectable.provided_in", format!("{:?}", expr_opt(&t.provided_in)), format!("{:?}", expr_opt(&b.provided_in)));
            push("injectable.use_class", format!("{:?}", expr_opt(&t.use_class)), format!("{:?}", expr_opt(&b.use_class)));
            push("injectable.use_factory", format!("{:?}", expr_opt(&t.use_factory)), format!("{:?}", expr_opt(&b.use_factory)));
            push("injectable.use_value", format!("{:?}", expr_opt(&t.use_value)), format!("{:?}", expr_opt(&b.use_value)));
            push("injectable.use_existing", format!("{:?}", expr_opt(&t.use_existing)), format!("{:?}", expr_opt(&b.use_existing)));
            let deps = |d: &Option<Vec<treaty_ivy_ng_bridge::facts::DepFact>>| {
                d.as_ref().map(|v| v.iter().map(dep_repr).collect::<Vec<_>>())
            };
            push("injectable.deps", format!("{:?}", deps(&t.deps)), format!("{:?}", deps(&b.deps)));
        }
        _ => push("injectable", format!("{}", t.is_some()), format!("{}", b.is_some())),
    }
}

fn diff_ng_module(
    t: Option<&NgModuleFacts>,
    b: Option<&NgModuleFacts>,
    push: &mut impl FnMut(&str, String, String),
) {
    match (t, b) {
        (None, None) => {}
        (Some(t), Some(b)) => {
            push("ng_module.declarations", format!("{:?}", t.declarations), format!("{:?}", b.declarations));
            push("ng_module.imports", format!("{:?}", t.imports), format!("{:?}", b.imports));
            push("ng_module.exports", format!("{:?}", t.exports), format!("{:?}", b.exports));
            push("ng_module.bootstrap", format!("{:?}", t.bootstrap), format!("{:?}", b.bootstrap));
            push("ng_module.providers", format!("{:?}", providers_opt(&t.providers)), format!("{:?}", providers_opt(&b.providers)));
            push("ng_module.schemas", format!("{:?}", t.schemas), format!("{:?}", b.schemas));
        }
        _ => push("ng_module", format!("{}", t.is_some()), format!("{}", b.is_some())),
    }
}


#[test]
fn ng_analyze_parity_over_corpus() {
    let root = corpus_root();
    if !root.is_dir() {
        eprintln!("skipping: Angular compliance corpus not found at {}", root.display());
        return;
    }
    // Deep templates need a large stack, as in the corpus dump.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || run(root))
        .expect("spawn parity thread");
    handle.join().expect("parity thread panicked");
}

fn run(root: PathBuf) {
    let mut files = Vec::new();
    collect(&root, &mut files);
    let mut report = Report::default();

    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else { continue };
        report.files += 1;
        let rel = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let treaty = decorator_facts_of_source(&source);
        let resources = siblings(&root, path);
        let bridge = treaty_ivy_ng_bridge::extract(&rel, &source, &resources);
        report.file_errors.extend(bridge.file_errors.iter().map(|e| format!("{rel}: {e}")));
        if treaty.is_empty() && bridge.classes.is_empty() {
            continue;
        }
        report.files_with_classes += 1;
        // Pair classes by (name, occurrence): Treaty reads top-level and exported classes only, so a
        // class ng-analyze reports that Treaty does not is a scope difference, not a field difference.
        let tmap = keyed(&treaty);
        let bmap = keyed(&bridge.classes);
        for key in bmap.keys() {
            if !tmap.contains_key(key) {
                let where_ = format!("{rel} :: {}", key.0);
                bump(&mut report.bridge_only_classes, "class-absent-from-treaty", || where_.clone());
            }
        }
        for (key, t) in &tmap {
            let where_ = format!("{rel} :: {}", key.0);
            let Some(b) = bmap.get(key) else {
                // ng-analyze reports no metadata for a class it analyses as JIT-only (`@NgModule({jit:
                // true})`). The emit path must fall back to Treaty's extractor for such a class.
                bump(&mut report.fallback_by_reason, "bridge-reports-no-class", || where_.clone());
                continue;
            };
            report.classes_compared += 1;
            match (t.unsupported.is_empty(), b.unsupported.is_empty()) {
                (true, true) => {
                    report.classes_both_extracted += 1;
                    for (field, tv, bv) in diff_class(t, b) {
                        let example = format!("{where_}: treaty {tv} | bridge {bv}");
                        bump(&mut report.diffs, &field, || example.clone());
                        match classify(&field, &rel, &source, &tv, &bv) {
                            Some(reason) => bump(&mut report.explained, reason, || example.clone()),
                            None => bump(&mut report.unexpected, &field, || example),
                        }
                    }
                }
                (false, true) => {
                    for reason in &t.unsupported {
                        let key = reason.split(':').next().unwrap_or(reason).to_string();
                        bump(&mut report.treaty_refused_bridge_extracted, &key, || format!("{where_}: {reason}"));
                    }
                    // Treaty refuses an identifier-valued `template` (`template: CONST`) that ngtsc
                    // evaluates to a string; the bridge extracts it. Anything else is unexplained.
                    for reason in &t.unsupported {
                        match explain_refusal(reason, source.as_str(), b) {
                            Some(why) => bump(&mut report.refusals_explained, why, || format!("{where_}: {reason}")),
                            None => bump(&mut report.unexpected, "treaty-refused-bridge-extracted", || format!("{where_}: {reason}")),
                        }
                    }
                }
                (true, false) => {
                    for reason in &b.unsupported {
                        let key = reason.split(':').next().unwrap_or(reason).to_string();
                        bump(&mut report.fallback_by_reason, &key, || format!("{where_}: {reason}"));
                    }
                }
                (false, false) => report.classes_both_refused += 1,
            }
        }
    }

    write_report(&report, files.len());
    assert!(
        report.unexpected.is_empty(),
        "un-allowlisted parity differences:\n{}",
        report
            .unexpected
            .iter()
            .map(|(k, (n, ex))| format!("  {k} x{n}: {ex}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn write_report(r: &Report, total_files: usize) {
    let Ok(path) = std::env::var("NG_ANALYZE_PARITY_REPORT") else {
        eprintln!(
            "ng-analyze parity: {} files, {} classes compared ({} with both extractors), {} diff fields, {} fallback groups",
            total_files,
            r.classes_compared,
            r.classes_both_extracted,
            r.diffs.len(),
            r.fallback_by_reason.len()
        );
        return;
    };
    let group = |m: &BTreeMap<String, (usize, String)>| {
        m.iter()
            .map(|(k, (n, ex))| format!("    {{\"key\": {k:?}, \"count\": {n}, \"example\": {ex:?}}}"))
            .collect::<Vec<_>>()
            .join(",\n")
    };
    let json = format!(
        "{{\n  \"corpusFiles\": {total_files},\n  \"filesWithClasses\": {},\n  \"classesCompared\": {},\n  \"classesBothExtracted\": {},\n  \"fallbackByReason\": [\n{}\n  ],\n  \"treatyRefusedBridgeExtracted\": [\n{}\n  ],\n  \"diffsByField\": [\n{}\n  ],\n  \"explainedDiffs\": [\n{}\n  ],\n  \"unexpectedFailures\": {},\n  \"classesBothRefused\": {},\n  \"fileErrors\": {},\n  \"fileErrorExamples\": {:?}\n}}\n",
        r.files_with_classes,
        r.classes_compared,
        r.classes_both_extracted,
        group(&r.fallback_by_reason),
        group(&r.refusals_explained),
        group(&r.diffs),
        group(&r.explained),
        r.unexpected.len(),
        r.classes_both_refused,
        r.file_errors.len(),
        r.file_errors.iter().take(3).collect::<Vec<_>>()
    );
    std::fs::write(&path, json).expect("write parity report");
    eprintln!("ng-analyze parity report written to {path}");
}
