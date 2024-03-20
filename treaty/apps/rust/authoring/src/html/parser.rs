use super::tokenizer::{Attribute, Token};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum DomNode {
    Element(ElementNode),
    Text(String),
    Comment(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct ElementNode {
    // And this
    pub tag_name: String,
    pub attributes: Vec<Attribute>,
    pub children: Vec<DomNode>,
}

impl fmt::Display for ElementNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Element({} [", self.tag_name)?;
        for (i, attr) in self.attributes.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", attr)?;
        }
        write!(f, "])")?;
        if !self.children.is_empty() {
            write!(f, " {{\n")?;
            for child in &self.children {
                write!(f, "    {}\n", child)?;
            }
            write!(f, "}}")?;
        }
        Ok(())
    }
}

impl fmt::Display for DomNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomNode::Element(element) => write!(f, "{}", element),
            DomNode::Text(text) => write!(f, "Text(\"{}\")", text),
            DomNode::Comment(comment) => write!(f, "Comment(\"{}\")", comment),
        }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    pub fn parse(&mut self) -> Vec<DomNode> {
        let mut nodes = Vec::new();

        while self.current < self.tokens.len() {
            match &self.tokens[self.current] {
                Token::Text(text) => {
                    nodes.push(DomNode::Text(text.clone()));
                    self.current += 1;
                }
                Token::Comment(comment) => {
                    nodes.push(DomNode::Comment(comment.clone()));
                    self.current += 1;
                }
                Token::StartTag(_, _) | Token::SelfClosingTag(_, _) => {
                    if let Some(node) = self.parse_element() {
                        nodes.push(node);
                    }
                }
                _ => self.current += 1,
            }
        }

        nodes
    }
    fn parse_element(&mut self) -> Option<DomNode> {
        if let Some(token) = self.tokens.get(self.current).cloned() {
            match token {
                Token::StartTag(tag_name, attributes)
                | Token::SelfClosingTag(tag_name, attributes) => {
                    self.current += 1;

                    let mut children = Vec::new();
                    while self.current < self.tokens.len() {
                        match &self.tokens[self.current] {
                            Token::EndTag(ref end_tag) if end_tag == &tag_name => {
                                self.current += 1;
                                break;
                            }
                            Token::Text(_) => {
                                if let Some(token) = self.tokens.get(self.current).cloned() {
                                    if let Token::Text(text) = token {
                                        children.push(DomNode::Text(text));
                                    }
                                }
                                self.current += 1;
                            }
                            _ => {
                                if let Some(child) = self.parse_element() {
                                    children.push(child);
                                } else {
                                    self.current += 1;
                                }
                            }
                        }
                    }

                    return Some(DomNode::Element(ElementNode {
                        tag_name,
                        attributes,
                        children,
                    }));
                }
                _ => self.current += 1,
            }
        }
        None
    }
}
