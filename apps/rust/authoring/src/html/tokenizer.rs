use std::fmt;

#[derive(Debug, PartialEq, Clone)]
pub enum Token {
    StartTag(String, Vec<Attribute>),
    EndTag(String),
    SelfClosingTag(String, Vec<Attribute>),
    Text(String),
    Comment(String),
}

#[derive(Debug, PartialEq, Clone)]
pub struct Attribute {
    name: String,
    value: String,
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}=\"{}\"", self.name, self.value)
    }
}

pub struct HtmlTokenizer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> HtmlTokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    pub fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        if self.pos >= self.input.len() {
            return None;
        }
        match self.input.as_bytes()[self.pos] as char {
            '<' if self.starts_with("<!--") => self.consume_comment(),
            '<' if self.starts_with("</") => self.consume_end_tag(),
            '<' => self.consume_start_or_self_closing_tag(),
            _ => self.consume_text(),
        }
    }

    fn skip_whitespace(&mut self) {
        self.pos += self.input[self.pos..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| c.len_utf8())
            .sum::<usize>();
    }

    fn starts_with(&self, start: &str) -> bool {
        if self.pos >= self.input.len() {
            return false;
        }
        self.input[self.pos..].starts_with(start)
    }

    fn consume_while<F>(&mut self, condition: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let start = self.pos;
        while self.pos < self.input.len()
            && condition(self.input[self.pos..].chars().next().unwrap())
        {
            self.pos += self.input[self.pos..].chars().next().unwrap().len_utf8();
        }
        self.input[start..self.pos].to_string()
    }

    fn consume_comment(&mut self) -> Option<Token> {
        self.pos += 4;

        let start_pos = self.pos;
        while !self.starts_with("-->") && self.pos < self.input.len() {
            self.pos += 1;
        }
        let comment = self.input[start_pos..self.pos].to_string();

        if self.starts_with("-->") {
            self.pos += 3;
            Some(Token::Comment(comment))
        } else {
            None
        }
    }

    fn consume_end_tag(&mut self) -> Option<Token> {
        self.pos += 2;
        let tag_name = self.consume_while(|c| c != '>');
        self.pos += 1;
        Some(Token::EndTag(tag_name))
    }

    fn consume_start_or_self_closing_tag(&mut self) -> Option<Token> {
        self.pos += 1;
        let tag_name = self.consume_while(|c| !c.is_whitespace() && c != '>' && c != '/');
        let attributes = self.consume_attributes();
        if self.starts_with("/>") {
            self.pos += 2;
            Some(Token::SelfClosingTag(tag_name, attributes))
        } else {
            self.pos += 1;
            Some(Token::StartTag(tag_name, attributes))
        }
    }

    fn consume_attributes(&mut self) -> Vec<Attribute> {
    let mut attributes = Vec::new();
    while self.pos < self.input.len() && !self.starts_with(">") && !self.starts_with("/>") {
        self.skip_whitespace();
        if self.starts_with(">") || self.starts_with("/>") {
            break;
        }
        let name = self.consume_while(|c| c != '=' && !c.is_whitespace());
        self.skip_whitespace();
        if self.pos < self.input.len() && self.input[self.pos..].starts_with("=") {
            self.pos += 1; // Skip '='
            self.skip_whitespace();
            if self.pos < self.input.len() {
                let quote = self.input[self.pos..].chars().next().unwrap();
                if quote == '"' || quote == '\'' {
                    self.pos += 1;
                    let value = self.consume_while(|c| c != quote);
                    self.pos += 1;
                    attributes.push(Attribute { name, value });
                }
            }
        }
    }
    attributes
}

    fn consume_text(&mut self) -> Option<Token> {
        let text = self.consume_while(|c| c != '<');
        Some(Token::Text(text.trim().to_string()))
    }
}
