//! Client-privacy redaction for emitted Source Map v3 JSON.
//!
//! render3 emits the v3 source map and embeds the *original authoring source* as the map's
//! `sourcesContent`. When a component declared a `server { … }` block, the authoring layer lifts
//! those server-only functions out of the client bundle — but their verbatim bodies must ALSO never
//! reach the client *map*. A leaked `sourcesContent` is just as much a disclosure as a leaked mapping.
//!
//! [`redact_server_bodies_in_map`] takes the emitted map JSON plus the verbatim server-fn body texts
//! the authoring layer already extracted (see [`crate::plugin::ServerFn::source`]) and blanks every
//! occurrence of those bodies inside each `sourcesContent` entry. The blanking PRESERVES line and
//! column positions — every redacted character is replaced with a space, and newlines are kept — so
//! the rest of the map (every `generated -> original` mapping into the surviving client text) stays
//! valid; only the server bytes become whitespace.
//!
//! When there are no server fns the map is returned unchanged.

use serde_json::Value;

/// Blank every server-fn body occurrence out of the map's `sourcesContent`, preserving positions.
///
/// `map_json` is the emitted Source Map v3 JSON (as produced by render3's `SourceMap::to_json`).
/// `server_bodies` are the verbatim source texts of the lifted server functions. Each body that
/// appears inside any `sourcesContent` string is overwritten in place with position-preserving
/// whitespace (every non-newline character becomes a space; `\n`/`\r` are kept), so:
///   * the server source text is GONE from `sourcesContent`, and
///   * every line/column in the map still resolves to the same place in the (now-blanked) content.
///
/// An empty `server_bodies` list returns `map_json` unchanged. A `map_json` that is empty or does not
/// parse as JSON is returned unchanged (the caller surfaces the underlying compile error separately).
pub fn redact_server_bodies_in_map(map_json: &str, server_bodies: &[String]) -> String {
    // Nothing to hide (no server block) or no map at all: pass through verbatim.
    if server_bodies.is_empty() || map_json.trim().is_empty() {
        return map_json.to_string();
    }
    // Only redact bodies that carry real content; an empty needle would match everywhere.
    let needles: Vec<&String> = server_bodies.iter().filter(|b| !b.is_empty()).collect();
    if needles.is_empty() {
        return map_json.to_string();
    }

    let Ok(mut value) = serde_json::from_str::<Value>(map_json) else {
        return map_json.to_string();
    };

    let Some(contents) = value.get_mut("sourcesContent").and_then(Value::as_array_mut) else {
        return map_json.to_string();
    };

    for entry in contents.iter_mut() {
        let Some(text) = entry.as_str() else { continue };
        let mut redacted = text.to_string();
        for needle in &needles {
            redacted = blank_all_occurrences(&redacted, needle);
        }
        *entry = Value::String(redacted);
    }

    serde_json::to_string(&value).unwrap_or_else(|_| map_json.to_string())
}

/// Build a Source Map v3 JSON document for an extracted plain-`.ts` SERVER module's CLIENT output,
/// with the server-fn bodies redacted out of its embedded `sourcesContent`.
///
/// The file-level `'use server'` (and `$$`-suffix) extraction path produces a client module that is a
/// transform of the original `.ts`: the server-fn declarations are lifted to the backend artifact and
/// each is re-exported as a typed RPC binding. The client must still ship a v3 map, and — exactly like
/// the inline `server { … }` path — that map must NOT carry the lifted server-fn bodies in its
/// `sourcesContent`.
///
/// This emits a minimal but valid v3 map: `sources = [source_name]`, `file = generated_name`, an empty
/// `names`/`mappings` (the additive byte-for-byte mapping is not threaded through the marker-extraction
/// pre-pass, so no per-token mappings are claimed), and `sourcesContent = [redacted original source]`,
/// where every `server_bodies` occurrence in the original source is blanked to position-preserving
/// whitespace via [`redact_server_bodies_in_map`]'s machinery. The server source text is therefore
/// absent from the client map's `sourcesContent`, while line/column positions of the surviving client
/// text are preserved.
pub fn client_map_with_redacted_source(
    original_source: &str,
    source_name: &str,
    generated_name: &str,
    server_bodies: &[String],
) -> String {
    // Blank each non-empty server body out of the embedded original source, preserving positions.
    let mut content = original_source.to_string();
    for body in server_bodies.iter().filter(|b| !b.is_empty()) {
        content = blank_all_occurrences(&content, body);
    }

    let map = Value::Object({
        let mut m = serde_json::Map::new();
        m.insert("version".to_string(), Value::from(3u8));
        m.insert("file".to_string(), Value::from(generated_name));
        m.insert("sources".to_string(), Value::Array(vec![Value::from(source_name)]));
        m.insert("sourcesContent".to_string(), Value::Array(vec![Value::from(content)]));
        m.insert("names".to_string(), Value::Array(Vec::new()));
        m.insert("mappings".to_string(), Value::from(""));
        m
    });
    serde_json::to_string(&map).unwrap_or_default()
}

/// Replace every occurrence of `needle` in `haystack` with position-preserving whitespace: each
/// matched character becomes a space, except `\n` and `\r`, which are preserved so line numbers (and
/// the byte length of the content) are unchanged. Non-overlapping, left-to-right.
fn blank_all_occurrences(haystack: &str, needle: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        // Keep everything before the match verbatim.
        out.push_str(&rest[..pos]);
        // Blank the match itself, preserving newlines (and total length in chars/bytes per char).
        out.push_str(&blank_text(needle));
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// Replace every character of `text` with a space, except newline characters, which are kept. This
/// preserves both the line count and the per-line column count of the original span while erasing its
/// content. (Each non-newline `char` maps to a single space, and a space is one UTF-16 code unit, so
/// column positions counted in UTF-16 units are preserved for the ASCII-dominated source that server
/// fns are written in.)
fn blank_text(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' { c } else { ' ' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with_content(content: &str) -> String {
        let v = serde_json::json!({
            "version": 3,
            "file": "out.js",
            "sources": ["app.ts"],
            "sourcesContent": [content],
            "names": [],
            "mappings": "",
        });
        serde_json::to_string(&v).unwrap()
    }

    fn content_of(map_json: &str) -> String {
        let v: Value = serde_json::from_str(map_json).unwrap();
        v["sourcesContent"][0].as_str().unwrap().to_string()
    }

    #[test]
    fn empty_server_bodies_passes_map_through_unchanged() {
        let map = map_with_content("const a = 1;\n");
        assert_eq!(redact_server_bodies_in_map(&map, &[]), map);
    }

    #[test]
    fn server_body_text_is_blanked_from_sources_content() {
        let body = "async function save(user) { return db.insert(user); }";
        let source = format!("import x from 'y';\n{body}\nconst keep = 1;\n");
        let map = map_with_content(&source);

        let out = redact_server_bodies_in_map(&map, &[body.to_string()]);
        let content = content_of(&out);

        // The server body text is GONE.
        assert!(!content.contains("db.insert"), "server body leaked in content: {content}");
        assert!(!content.contains("async function save"), "server signature leaked: {content}");
        // The surrounding client text survives verbatim.
        assert!(content.contains("import x from 'y';"), "client import lost: {content}");
        assert!(content.contains("const keep = 1;"), "client tail lost: {content}");
    }

    #[test]
    fn redaction_preserves_line_and_column_positions() {
        let body = "function f() { secret(); }";
        let source = format!("a;\n{body}\nb;\n");
        let map = map_with_content(&source);

        let out = redact_server_bodies_in_map(&map, &[body.to_string()]);
        let content = content_of(&out);

        // Same total length (positions preserved) and same line count.
        assert_eq!(content.len(), source.len(), "byte length changed");
        assert_eq!(
            content.lines().count(),
            source.lines().count(),
            "line count changed"
        );
        // The body's line is now all spaces (no surviving identifier chars).
        let blanked_line = content.lines().nth(1).unwrap();
        assert_eq!(blanked_line, " ".repeat(body.len()), "body line not fully blanked: {blanked_line:?}");
    }

    #[test]
    fn non_json_map_is_returned_unchanged() {
        let bogus = "not a map";
        assert_eq!(redact_server_bodies_in_map(bogus, &["x".to_string()]), bogus);
    }

    #[test]
    fn map_without_sources_content_is_returned_unchanged() {
        let map = serde_json::json!({ "version": 3, "mappings": "" }).to_string();
        assert_eq!(redact_server_bodies_in_map(&map, &["x".to_string()]), map);
    }

    #[test]
    fn client_map_builder_redacts_bodies_and_stays_valid_v3() {
        let body = "async function loadSecret() { return 'postgres://secret'; }";
        let source = format!("'use server'\n{body}\nexport const k = 1;\n");

        let map = client_map_with_redacted_source(&source, "x.ts", "x.js", &[body.to_string()]);
        let value: Value = serde_json::from_str(&map).expect("valid JSON");

        assert_eq!(value["version"], serde_json::json!(3));
        assert_eq!(value["file"], serde_json::json!("x.js"));
        assert_eq!(value["sources"], serde_json::json!(["x.ts"]));

        let content = value["sourcesContent"][0].as_str().unwrap();
        // Body gone, surrounding text preserved, length unchanged (positions intact).
        assert!(!content.contains("postgres://secret"), "secret leaked: {content}");
        assert!(!content.contains("async function loadSecret"), "signature leaked: {content}");
        assert!(content.contains("export const k = 1;"), "tail lost: {content}");
        assert_eq!(content.len(), source.len(), "length changed (positions broken)");
    }

    #[test]
    fn client_map_builder_with_no_bodies_embeds_source_verbatim() {
        let source = "export const add = (a, b) => a + b;\n";
        let map = client_map_with_redacted_source(source, "m.ts", "m.js", &[]);
        let value: Value = serde_json::from_str(&map).expect("valid JSON");
        assert_eq!(value["sourcesContent"][0].as_str().unwrap(), source);
    }

    #[test]
    fn multiple_server_bodies_all_blanked() {
        let a = "function a() { return secretA; }";
        let b = "function b() { return secretB; }";
        let source = format!("{a}\nmiddle;\n{b}\n");
        let map = map_with_content(&source);

        let out = redact_server_bodies_in_map(&map, &[a.to_string(), b.to_string()]);
        let content = content_of(&out);
        assert!(!content.contains("secretA"), "body A leaked: {content}");
        assert!(!content.contains("secretB"), "body B leaked: {content}");
        assert!(content.contains("middle;"), "middle client text lost: {content}");
    }
}
