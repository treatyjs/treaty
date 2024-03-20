pub struct ParsedContent {
  pub html: Vec<String>,
  pub css: Vec<String>,
  pub javascript: Vec<String>,
}

pub fn parse_mixed_content(content: &str) -> Result<ParsedContent, String> {
    let mut in_style = false;
    let mut in_html = false;
    let mut css_content = Vec::new();
    let mut html_content = Vec::new();
    let mut javascript_content = Vec::new();
    let mut current_block = String::new();

    for line in content.lines() {
        if line.trim_start().starts_with("<style>") {
            if in_html || in_style {
                return Err("Found <style> tag inside another block".to_string());
            }
            in_style = true;
            continue;
        }
        if line.trim_start().starts_with("</style>") {
            if !in_style {
                return Err("Mismatched </style> tag found".to_string());
            }
            in_style = false;
            css_content.push(current_block.trim().to_string());
            current_block.clear();
            continue;
        }
        if line.trim().starts_with("<") && !line.trim().starts_with("</") {
            if !in_html {
                return Err("Found starting HTML tag inside another block".to_string());
            }
            in_html = true;
        } else if in_html && line.contains(">") {
            in_html = false;
            html_content.push(current_block.trim().to_string() + line);
            current_block.clear();
            continue;
        }

        if in_style || in_html {
            current_block.push_str(line);
            current_block.push('\n');
        } else {
            // Treat as JavaScript if not within an HTML or CSS block
            javascript_content.push(line.to_string());
        }
    }

    // Check for unclosed tags
    if in_html {
        return Err("Unclosed HTML tag".to_string());
    }
    if in_style {
        return Err("Unclosed <style> tag".to_string());
    }

    Ok(ParsedContent {
        css: css_content,
        html: html_content,
        javascript: javascript_content,
    })
}