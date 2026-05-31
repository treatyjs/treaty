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
