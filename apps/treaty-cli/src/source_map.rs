//! Dev source-map composition for `treaty serve`.
//!
//! The Treaty pipeline produces a served module in TWO stages, each with its own
//! position space:
//!
//!   1. The compiler (`treaty_ivy` / `rust_authoring`) Ivy-lowers the authoring
//!      source to **Ivy-but-still-TypeScript** and hands back an additive v3 map
//!      `Ivy-TS -> original .ts/.treaty` (`sources[0]` is the authoring file,
//!      `sourcesContent[0]` is its text). This module NEVER recomputes that map —
//!      it only CONSUMES the facade `{code, map}` entry.
//!   2. [`crate::transform::strip_types`] type-strips the Ivy-TS to browser ESM
//!      with `oxc_codegen`, which (with its sourcemap feature) emits a second v3
//!      map `stripped-ESM -> Ivy-TS`.
//!
//! Browser DevTools only ever sees the served ESM, so it needs ONE map
//! `stripped-ESM -> original`. [`compose`] threads stage 2 through stage 1: for
//! every stage-2 token (a position in the served ESM that points at a position in
//! the Ivy-TS), it looks that Ivy-TS position up in the stage-1 facade map to
//! recover the ORIGINAL authoring position, and emits a token straight from the
//! served ESM to the `.ts`/`.treaty`. The result carries the authoring file in
//! `sources[]` + its text in `sourcesContent[]`, so a click in DevTools lands on
//! the real source line.
//!
//! When the compiler emitted no facade map (a pass-through `.mjs`, or a front-end
//! that returned `None`), [`compose`] falls back to the stage-2 Codegen map alone
//! — still a valid map, just pointing at the Ivy-TS intermediate rather than the
//! authoring file. When NEITHER map exists there is nothing to serve and the
//! caller omits the `sourceMappingURL` entirely.

use oxc_sourcemap::{SourceMap, Token};

/// Compose `stripped -> ivy_ts` (the type-strip Codegen map) with
/// `ivy_ts -> original` (the compiler's additive facade map) into one
/// `stripped -> original` map, returned as inline base64 `data:` URL ready to
/// append after `//# sourceMappingURL=`.
///
/// * `codegen_map` — the `oxc_codegen` map from the type-strip pass, or `None`
///   when the stripped code carried no spans (e.g. an empty module).
/// * `facade_map_json` — the compiler's v3 map JSON (`Ivy-TS -> original`), or
///   `None` for a pass-through module the compiler did not map.
/// * `original_source` — the real authoring file path (the served file's source).
///   The facade front-ends stamp a placeholder `sources[0]` (e.g. `"component.ts"`)
///   because they are not threaded the on-disk path; the dev server KNOWS it, so
///   it re-homes the single source onto this path (its `sourcesContent` — the
///   original text, already embedded by the compiler — is preserved verbatim) so
///   DevTools resolves a click to the real `.ts`/`.treaty`.
///
/// Returns `None` only when BOTH inputs are absent (nothing to map). When only
/// the facade map is absent, the Codegen map is emitted as-is (still valid).
pub fn compose(
    codegen_map: Option<SourceMap>,
    facade_map_json: Option<&str>,
    original_source: &str,
) -> Option<String> {
    let facade = facade_map_json.and_then(|json| SourceMap::from_json_string(json).ok());

    match (codegen_map, facade) {
        // Both present: compose stage2 ∘ stage1 token-by-token, re-homing the
        // single facade source onto the real authoring path.
        (Some(cg), Some(facade)) => {
            let composed = compose_maps(&cg, &facade, original_source);
            Some(composed.to_data_url())
        }
        // Only the Codegen map: serve it directly (points at the Ivy-TS source).
        (Some(mut cg), None) => {
            cg.set_file(original_source);
            Some(cg.to_data_url())
        }
        // Only the facade map (no type-strip happened, e.g. a linked vendor module
        // we still mapped): serve it directly, re-homed onto the real source.
        (None, Some(mut facade)) => {
            rehome_sources(&mut facade, original_source);
            facade.set_file(original_source);
            Some(facade.to_data_url())
        }
        (None, None) => None,
    }
}

/// Replace a single-source map's `sources[0]` with `original_source` (keeping its
/// `sourcesContent`). The Treaty front-ends always emit exactly ONE source (the
/// authoring file) per module, so this targets index 0; a multi-source map (never
/// produced here) is left untouched.
fn rehome_sources(map: &mut SourceMap, original_source: &str) {
    let count = map.get_sources().count();
    if count == 1 {
        map.set_sources([original_source]);
    }
}

/// The token-composition core: walk every `stripped -> ivy_ts` token, resolve its
/// Ivy-TS position through the facade map's lookup table to an `original`
/// position, and build a fresh map straight from the stripped ESM to the
/// authoring source.
fn compose_maps(codegen: &SourceMap, facade: &SourceMap, original_source: &str) -> SourceMap {
    // The facade map has exactly one source (the authoring file) carried with its
    // content; we re-home every composed token onto that single source so the
    // emitted map's `sources`/`sourcesContent` are the original `.ts`/`.treaty`.
    let facade_lookup = facade.generate_lookup_table();

    // Re-home `sources[0]` onto the real authoring path the dev server knows; keep
    // the compiler-embedded `sourcesContent` (the original text) verbatim. A
    // single-source map is the only shape the Treaty front-ends emit.
    let facade_sources_len = facade.get_sources().count();
    let sources: Vec<std::sync::Arc<str>> = if facade_sources_len == 1 {
        vec![original_source.into()]
    } else {
        facade.get_sources().cloned().collect()
    };
    let source_contents: Vec<Option<std::sync::Arc<str>>> = facade
        .get_source_contents()
        .map(|c| c.cloned())
        .collect();

    // Names are recovered from the facade map (it names the original identifiers);
    // collect them so a composed token can reference them by index.
    let mut names: Vec<std::sync::Arc<str>> = Vec::new();
    let mut name_index = std::collections::HashMap::<String, u32>::new();

    let mut tokens: Vec<Token> = Vec::new();
    for cg_tok in codegen.get_tokens() {
        // `cg_tok`: dst = position in the served (stripped) ESM, src = position in
        // the Ivy-TS. Resolve that Ivy-TS position through the facade map.
        let ivy_line = cg_tok.get_src_line();
        let ivy_col = cg_tok.get_src_col();
        let Some(orig) = facade.lookup_token(&facade_lookup, ivy_line, ivy_col) else {
            // No original position for this Ivy-TS span (compiler-synthesized
            // code with no authoring origin): drop the token. DevTools simply has
            // no mapping there, which is correct — it is generated boilerplate.
            continue;
        };

        // Carry the original identifier name when the facade token named one.
        let name_id = orig.get_name_id().and_then(|id| facade.get_name(id)).map(|n| {
            let key = n.to_string();
            *name_index.entry(key).or_insert_with(|| {
                let idx = names.len() as u32;
                names.push(n.clone());
                idx
            })
        });

        tokens.push(Token::new(
            cg_tok.get_dst_line(),
            cg_tok.get_dst_col(),
            orig.get_src_line(),
            orig.get_src_col(),
            // Single source (the authoring file) -> index 0 when present.
            orig.get_source_id().map(|_| 0).or(Some(0)),
            name_id,
        ));
    }

    SourceMap::new(
        // `file` is the generated artifact label; the served JS shares the source
        // stem, so the authoring path is a faithful, informational `file` value.
        Some(original_source.into()),
        names,
        None,
        sources,
        source_contents,
        tokens.into_boxed_slice(),
        None,
    )
}

/// Append `//# sourceMappingURL=<data-url>` to a served module body. The marker
/// is on its own final line so DevTools picks it up regardless of trailing code.
pub fn append_inline(body: &str, data_url: &str) -> String {
    // Avoid a double newline if the body already ends with one.
    let sep = if body.ends_with('\n') { "" } else { "\n" };
    format!("{body}{sep}//# sourceMappingURL={data_url}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A facade map JSON that maps Ivy-TS line/col back to an original `.ts`.
    /// Mapping a single token at Ivy-TS (0,0) -> original (3,7) in `app.ts`.
    fn facade_json() -> String {
        // version 3, one source with content, one mapping segment at dst (0,0)
        // pointing at source 0, line 3, col 7. VLQ for [0,0,3,7] = "AAGO".
        // We build it programmatically to avoid hand-encoding VLQ.
        let map = SourceMap::new(
            Some("app.js".into()),
            vec![],
            None,
            vec!["app.ts".into()],
            vec![Some("import { Component } from '@angular/core';\n@Component({})\nexport class App { name = 'x'; }\n".into())],
            vec![Token::new(0, 0, 3, 7, Some(0), None)].into_boxed_slice(),
            None,
        );
        map.to_json_string()
    }

    /// A Codegen-style map: stripped ESM (5,2) maps to Ivy-TS (0,0).
    fn codegen_map() -> SourceMap {
        SourceMap::new(
            Some("app.js".into()),
            vec![],
            None,
            vec!["app.ivy.ts".into()],
            vec![Some("ivy-ts-source".into())],
            vec![Token::new(5, 2, 0, 0, Some(0), None)].into_boxed_slice(),
            None,
        )
    }

    #[test]
    fn compose_threads_stripped_through_ivy_to_original() {
        // The facade map stamps a placeholder `app.ts`; the dev server passes the
        // REAL authoring path, which re-homes `sources[0]` while preserving
        // `sourcesContent` and threading positions to the original.
        let data_url = compose(Some(codegen_map()), Some(&facade_json()), "src/app.ts")
            .expect("composed map");
        assert!(data_url.starts_with("data:application/json"), "not a data url: {data_url}");
        // Decode the base64 payload and assert the original source survived.
        let b64 = data_url.rsplit("base64,").next().unwrap();
        let bytes = base64_decode(b64);
        let json = String::from_utf8(bytes).unwrap();
        let decoded = SourceMap::from_json_string(&json).expect("valid v3");
        let sources: Vec<String> = decoded.get_sources().map(|s| s.to_string()).collect();
        assert_eq!(sources, vec!["src/app.ts".to_string()], "source not re-homed: {json}");
        // The re-homed source must still carry the ORIGINAL authoring text.
        let content: Vec<Option<String>> =
            decoded.get_source_contents().map(|c| c.map(|s| s.to_string())).collect();
        assert!(
            content.first().and_then(|c| c.as_ref()).is_some_and(|c| c.contains("class App")),
            "lost sourcesContent: {content:?}"
        );
        // The single composed token must land on the ORIGINAL position (3,7),
        // not the Ivy-TS (0,0), at the stripped dst (5,2).
        let tok = decoded.get_token(0).expect("one token");
        assert_eq!((tok.get_dst_line(), tok.get_dst_col()), (5, 2));
        assert_eq!((tok.get_src_line(), tok.get_src_col()), (3, 7), "did not thread to original");
    }

    #[test]
    fn compose_without_facade_falls_back_to_codegen_map() {
        // No facade map: the Codegen map is served as-is (no re-home; it already
        // points at the Ivy-TS intermediate), with `file` set to the served name.
        let data_url = compose(Some(codegen_map()), None, "app.ivy.ts").expect("map");
        let b64 = data_url.rsplit("base64,").next().unwrap();
        let json = String::from_utf8(base64_decode(b64)).unwrap();
        let decoded = SourceMap::from_json_string(&json).unwrap();
        let sources: Vec<String> = decoded.get_sources().map(|s| s.to_string()).collect();
        assert_eq!(sources, vec!["app.ivy.ts".to_string()]);
    }

    #[test]
    fn compose_without_any_map_is_none() {
        assert!(compose(None, None, "app.ts").is_none());
    }

    #[test]
    fn append_inline_terminates_with_marker_line() {
        let out = append_inline("const x = 1;", "data:application/json;base64,AAA");
        assert!(out.contains("//# sourceMappingURL=data:application/json;base64,AAA"));
        assert!(out.ends_with('\n'));
    }

    /// Minimal standard base64 decoder for the test (avoids pulling a dep into the
    /// test just to verify the data-url payload).
    fn base64_decode(s: &str) -> Vec<u8> {
        const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| TBL.iter().position(|&t| t == c).map(|p| p as u32);
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &c in s.as_bytes() {
            if c == b'=' {
                break;
            }
            if let Some(v) = val(c) {
                buf = (buf << 6) | v;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((buf >> bits) as u8);
                }
            }
        }
        out
    }
}
