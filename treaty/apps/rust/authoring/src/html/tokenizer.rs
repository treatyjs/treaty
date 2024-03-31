use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ControlFlowKind {
    IfElse {
        condition: String,
        body: Vec<TokenKind>,
        else_body: Option<Box<ControlFlowKind>>,
    },
    For {
        iterator: String,
        body: Vec<TokenKind>,
    },
    Switch {
        expression: String,
        cases: Vec<(String, Vec<TokenKind>)>, // (case value, body tokens)
        default_case: Option<Vec<TokenKind>>,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    StartTag {
        name: String,
        attributes: Vec<Attribute>,
        is_self_closing: bool,
    },
    EndTag(String),
    Text(String),
    Comment(String),
    TemplateExpression(String),
    ControlFlow(ControlFlowKind),

}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub pos: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub value: String,
    pub pos: Position,
}
impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}=\"{}\"", self.name, self.value)
    }
}


pub struct HtmlTokenizer<'a> {
    input: &'a str,
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> HtmlTokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if !c.is_whitespace() {
                break;
            }
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn starts_with(&self, start: &str) -> bool {
        self.input[self.pos..].starts_with(start)
    }

    pub fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        let start_pos = Position {
            line: self.line,
            col: self.col,
        };
        match self.peek()? {
            '<' if self.starts_with("<!--") => self.consume_comment(),
            '<' if self.starts_with("</") => self.consume_end_tag(),
            '<' => self.consume_start_tag(),
            '@' if self.starts_with("@if") => self.parse_if(),
            '@' if self.starts_with("@for") => self.parse_for(),
            '@' if self.starts_with("@switch") => self.switch(),
            _ => self.consume_text(),
        }
        .map(|kind| Token {
            kind,
            pos: start_pos,
        })
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


    fn consume_comment(&mut self) -> Option<TokenKind> {
        self.pos += 4; // Skip <!--
        let comment_end = "-->";
        let end_pos = self.input[self.pos..]
            .find(comment_end)
            .unwrap_or(self.input.len());
        let comment = self.input[self.pos..self.pos + end_pos].to_string();
        self.pos += end_pos + comment_end.len();
        Some(TokenKind::Comment(comment))
    }

    fn consume_end_tag(&mut self) -> Option<TokenKind> {
        self.pos += 2; // Skip </
        let tag_name = self.consume_while(|c| c != '>');
        self.pos += 1; // Skip >
        Some(TokenKind::EndTag(tag_name))
    }

    fn consume_start_tag(&mut self) -> Option<TokenKind> {
        self.pos += 1; // Skip <
        let tag_name = self.consume_while(|c| !c.is_whitespace() && c != '>' && c != '/');
        let attributes = self.consume_attributes();
        let is_self_closing = if self.starts_with("/>") {
            self.pos += 2;
            true
        } else {
            self.pos += 1;
            false
        };
        Some(TokenKind::StartTag {
            name: tag_name,
            attributes,
            is_self_closing,
        })
    }

    fn consume_attributes(&mut self) -> Vec<Attribute> {
        let mut attributes = Vec::new();
        loop {
            self.skip_whitespace();
            if self.starts_with(">") || self.starts_with("/>") {
                break;
            }
            let name = self.consume_while(|c| c != '=' && !c.is_whitespace());
            self.skip_whitespace();
            self.pos += 1; // Skip =
            self.skip_whitespace();
            let quote = self.peek().unwrap();
            self.pos += 1; // Skip quote
            let value = self.consume_while(|c| c != quote);
            self.pos += 1; // Skip quote
            attributes.push(Attribute {
                name,
                value,
                pos: Position {
                    line: self.line,
                    col: self.col,
                },
            });
        }
        attributes
    }

    fn consume_text(&mut self) -> Option<TokenKind> {
        let text = self.consume_while(|c| c != '<');
        if !text.is_empty() {
            Some(TokenKind::Text(text))
        } else {
            None
        }
    }

    fn consume_while<F>(&mut self, condition: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let mut result = String::new();
        while let Some(c) = self.peek() {
            if !condition(c) {
                break;
            }
            result.push(c);
            self.pos += c.len_utf8();
            self.update_position(c);
        }
        result
    }

    fn update_position(&mut self, c: char) {
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
    }
}
