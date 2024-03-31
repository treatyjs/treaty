use super::tokenizer::{Attribute, Position, Token, TokenKind};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum DomNode {
    Element(ElementNode),
    Text(String, Position),
    Comment(String, Position),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementNode {
    pub tag_name: String,
    pub attributes: Vec<Attribute>,
    pub children: Vec<DomNode>,
    pub pos: Position,
    pub is_self_closing: bool,
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
            DomNode::Text(text, _) => write!(f, "Text(\"{}\")", text),
            DomNode::Comment(comment, _) => write!(f, "Comment(\"{}\")", comment),
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
        let (kind, pos) = {
            let token = &self.tokens[self.current];
            (token.kind.clone(), token.pos.clone())
        };

        match kind {
            TokenKind::Text(content) => {
                nodes.push(DomNode::Text(content, pos));
                self.current += 1;
            },
            TokenKind::Comment(content) => {
                nodes.push(DomNode::Comment(content, pos));
                self.current += 1;
            },
            TokenKind::StartTag { name, attributes, is_self_closing } => {
                if let Some(node) = self.parse_element(&name, &attributes, is_self_closing) {
                    nodes.push(node);
                }
            },
            _ => self.current += 1,
        }
    }

    nodes
}

    fn parse_element(
        &mut self,
        tag_name: &str,
        attributes: &Vec<Attribute>,
        is_self_closing: bool,
    ) -> Option<DomNode> {
        let start_pos = self.tokens[self.current].pos.clone();

        let mut children = Vec::new();
        if !is_self_closing {
            self.current += 1;

            while self.current < self.tokens.len() {
                match self.tokens[self.current].kind {
                    TokenKind::EndTag(ref name) if name == tag_name => {
                        self.current += 1;
                        break;
                    }
                    _ => {
                        let child = self.parse();
                        children.extend(child);
                    }
                }
            }
        } else {
            self.current += 1;
        }

        Some(DomNode::Element(ElementNode {
            tag_name: tag_name.to_string(),
            attributes: attributes.clone(),
            children,
            pos: start_pos,
            is_self_closing,
        }))
    }
}
