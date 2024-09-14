use crate::html::{DomNode, HtmlTokenizer, Parser};
use std::fmt;
#[derive(Debug, PartialEq, Clone)]
pub enum ControlFlowContent {
    IfElse(String, Vec<HtmlContent>, Option<Box<ControlFlowContent>>),
    For(String, Vec<HtmlContent>, Option<Vec<HtmlContent>>),
    Switch(
        String,
        Vec<(String, Vec<HtmlContent>)>,
        Option<Vec<HtmlContent>>,
    ),
}

#[derive(Debug, PartialEq, Clone)]
pub enum HtmlContent {
    Node(DomNode),
    TemplateExpression(String),
    ControlFlow(ControlFlowContent),
}
impl HtmlContent {
    fn into_control_flow_content(self) -> Option<ControlFlowContent> {
        match self {
            HtmlContent::ControlFlow(content) => Some(content),
            _ => None,
        }
    }
}

impl fmt::Display for HtmlContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HtmlContent::Node(node) => write!(f, "{}", node),
            HtmlContent::TemplateExpression(expression) => {
                write!(f, "TemplateExpression({})", expression)
            }
            HtmlContent::ControlFlow(content) => write!(f, "ControlFlow({:?})", content),
        }
    }
}

pub enum Token {
    Style(String),
    JavaScript(String),
    HTML(Vec<HtmlContent>),
}

pub struct Lexer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str, ) -> Self {
        Lexer {
            input,
            pos: 0,
        }
    }

    pub fn for_each_token<F>(&mut self, mut callback: F)
    where
        F: FnMut(Token),
    {
        while let Some(token) = self.next_token() {
            callback(token);
        }
    }

    pub fn next_token(&mut self) -> Option<Token> {
        while let Some(current_char) = self.peek() {
            match current_char {
                '<' if self.lookahead("<style>") => {
                    self.advance(7); // Skip past <style>
                    return self.parse_style();
                }
                '<' => {
                    return self.parse_html_segment();
                }
                '{' if self.lookahead("{{") => {
                    self.advance(2); // Skip past {{
                    let expression = self.parse_template_expression()?;
                    return Some(Token::HTML(vec![HtmlContent::TemplateExpression(
                        expression,
                    )]));
                }
                '@' => {
                    let control_flow = self.parse_control_flow()?;
                    return Some(Token::HTML(vec![control_flow]));
                }
                '-' if self.lookahead("---") => {
                    // this is 100% javascript/Typescript
                    self.advance(3); // Skip past ---

                    return self.parse_javascript();
                }
                _ => {
                    // Handles mix files
                    return self.parse_text();
                }
            }
        }
        None
    }

    fn parse_control_flow(&mut self) -> Option<HtmlContent> {
        match self.peek_keyword().as_deref() {
            Some("if") => self.parse_if(),
            Some("for") => self.parse_for(),
            Some("switch") => self.parse_switch(),
            _ => None,
        }
    }

    fn parse_template_expression(&mut self) -> Option<String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '}' && self.lookahead("}}") {
                let expression = &self.input[start..self.pos];
                self.advance(2);
                return Some(expression.trim().to_string());
            }
            self.advance(1);
        }
        None
    }

    fn parse_html_segment(&mut self) -> Option<Token> {
        let mut tag_depth = 0;
        let mut start = self.pos;
        let mut end = self.pos;
        let mut is_in_tag = false;

        while let Some(ch) = self.peek() {
            if ch == '<' {
                is_in_tag = true;
                if self.lookahead("</") {
                    tag_depth -= 1;
                    if tag_depth == 0 {
                        self.advance_while(|c| c != '>');
                        self.advance(1);
                        end = self.pos;
                        break;
                    }
                } else {
                    if tag_depth == 0 {
                        start = self.pos;
                    }
                    tag_depth += 1;
                }
            } else if ch == '>' && is_in_tag {
                is_in_tag = false;
            }
            self.advance(1);
            if tag_depth == 0 && !is_in_tag {
                end = self.pos;
            }
        }

        let html_content = &self.input[start..end];
        println!("Processing HTML segment: {}", html_content);

        let mut html_tokens = HtmlTokenizer::new(html_content);
        let mut tokens = Vec::new();
        while let Some(token) = html_tokens.next_token() {
            tokens.push(token);
        }
        let mut html_parser = Parser::new(tokens);
        let dom_nodes = html_parser.parse();

        Some(Token::HTML(
            dom_nodes.into_iter().map(HtmlContent::Node).collect(),
        ))
    }

    fn advance_while<F>(&mut self, condition: F)
    where
        F: Fn(char) -> bool,
    {
        while let Some(ch) = self.peek() {
            if condition(ch) {
                self.advance(1);
            } else {
                break;
            }
        }
    }

    fn parse_if(&mut self) -> Option<HtmlContent> {
        self.consume_keyword("if");
        let condition = self.capture_until('{');
        self.advance(1);
        let content = self.capture_block();
        self.advance(1);

        let mut else_content: Option<Box<ControlFlowContent>> = None;
        if self.lookahead("@else") {
            self.advance(5);
            if self.lookahead(" if") {
                // Check for @else if
                self.advance(3);
                else_content = Some(Box::new(
                    self.parse_if()?.into_control_flow_content().unwrap(),
                ));
            } else {
                self.advance(1);
                let else_block = self.capture_block();
                self.advance(1);
                else_content = Some(Box::new(ControlFlowContent::IfElse(
                    "".to_string(),
                    else_block,
                    None,
                )));
            }
        }

        Some(HtmlContent::ControlFlow(ControlFlowContent::IfElse(
            condition,
            content,
            else_content,
        )))
    }

    fn parse_for(&mut self) -> Option<HtmlContent> {
        self.consume_keyword("for");
        let iterator = self.capture_until('{');
        self.advance(1);
        let content = self.capture_block();
        self.advance(1);

        let empty_content = if self.lookahead("@empty") {
            self.consume_keyword("empty");
            Some(self.capture_block())
        } else {
            None
        };

        Some(HtmlContent::ControlFlow(ControlFlowContent::For(
            iterator,
            content,
            empty_content,
        )))
    }

    fn parse_switch(&mut self) -> Option<HtmlContent> {
        self.consume_keyword("switch");
        let expression = self.capture_until('{');
        self.advance(1);

        let mut cases = Vec::new();
        let mut default_case: Option<Vec<HtmlContent>> = None;
        while let Some(keyword) = self.peek_keyword() {
            match keyword.as_str() {
                "case" => {
                    self.consume_keyword("case");
                    let case_expr = self.capture_until('{');
                    self.advance(1);
                    let case_content = self.capture_block();
                    self.advance(1);
                    cases.push((case_expr, case_content));
                }
                "default" => {
                    self.consume_keyword("default");
                    self.advance(1);
                    default_case = Some(self.capture_block());
                    self.advance(1);
                    break;
                }
                _ => break,
            }
        }

        Some(HtmlContent::ControlFlow(ControlFlowContent::Switch(
            expression,
            cases,
            default_case,
        )))
    }

    fn capture_until(&mut self, until: char) -> String {
        let start_pos = self.pos;
        while let Some(current_char) = self.peek() {
            if current_char == until {
                break;
            }
            self.advance(1);
        }
        self.input[start_pos..self.pos].to_string()
    }

    fn consume_keyword(&mut self, keyword: &str) {
        let len = keyword.len();
        if self.input[self.pos..].starts_with(keyword) {
            self.pos += len; // Move past the keyword
            self.consume_whitespace(); // Consume any following whitespace
        }
    }

    fn capture_block(&mut self) -> Vec<HtmlContent> {
        let mut block_contents = Vec::new();
        let mut block_depth = 1;
        let block_start = self.pos;

        while let Some(ch) = self.peek() {
            match ch {
                '{' => {
                    block_depth += 1;
                    self.advance(1);
                }
                '}' => {
                    block_depth -= 1;
                    if block_depth == 0 {
                        let content = self.input[block_start..self.pos].trim().to_string();
                        if !content.is_empty() {
                            block_contents.push(HtmlContent::Node(DomNode::Text(content)));
                        }
                        self.advance(1);
                        break;
                    }
                    self.advance(1);
                }
                _ => self.advance(1),
            }
        }

        if block_depth != 0 {
            panic!("Unmatched braces in block content");
        }

        block_contents
    }

    fn consume_whitespace(&mut self) {
        while let Some(current_char) = self.peek() {
            if current_char.is_whitespace() {
                self.advance(1);
            } else {
                break;
            }
        }
    }

    fn peek_keyword(&mut self) -> Option<String> {
        let current_pos = self.pos;
        let mut keyword = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || !ch.is_alphanumeric() {
                break;
            }
            keyword.push(ch);
            self.advance(1);
        }
        self.pos = current_pos;
        if !keyword.is_empty() {
            Some(keyword)
        } else {
            None
        }
    }

    fn parse_style(&mut self) -> Option<Token> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '<' && self.lookahead("</style>") {
                break;
            }
            self.advance(1);
        }
        let style_content = &self.input[start..self.pos];
        self.advance(8);
        Some(Token::Style(style_content.to_string()))
    }

    fn parse_javascript(&mut self) -> Option<Token> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '-' && self.lookahead("---") {
                break;
            }
            self.advance(1);
        }
        let javascript_content = &self.input[start..self.pos];
        self.advance(3);
        Some(Token::JavaScript(javascript_content.trim().to_owned()))
    }

    fn parse_text(&mut self) -> Option<Token> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '<' || (ch == '{' && self.lookahead("{{")) {
                break;
            }
            self.advance(1);
        }
        let text = &self.input[start..self.pos];
        Some(Token::JavaScript(text.trim().to_owned()))
    }


    fn peek(&self) -> Option<char> {
        self.input.get(self.pos..).and_then(|s| s.chars().next())
    }

    fn lookahead(&self, expected: &str) -> bool {
        self.input[self.pos..].starts_with(expected)
    }

    fn advance(&mut self, by: usize) {
        self.pos += by;
    }
}
