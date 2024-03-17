

use crate::treaty::{Lexer, Token, HtmlContent};

pub struct ParsedContent {
    pub html: Vec<HtmlContent>,
    pub css: Vec<String>,
    pub javascript: Vec<String>,
}

pub fn parse_mixed_content<'a>(input: &'a str) -> ParsedContent {
    let mut lexer = Lexer::new(input);
    let mut parsed_content = ParsedContent { html: Vec::new(), css: Vec::new(), javascript: Vec::new(), };

    lexer.for_each_token(|token| match token {
        Token::HTML(html_content) => parsed_content.html.extend(html_content),
        Token::Style(css_content) => parsed_content.css.push(css_content),
        Token::JavaScript(js_content) => parsed_content.javascript.push(js_content),

    });

    parsed_content
}
