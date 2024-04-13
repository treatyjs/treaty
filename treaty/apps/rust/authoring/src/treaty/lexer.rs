use crate::html::{DomNode, HtmlTokenizer, Parser};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

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
    three_parser: tree_sitter::Parser,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Lexer {
            input,
            pos: 0,
            three_parser: tree_sitter::Parser::new(),
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

   fn generate_ivy_code(&mut self, node: tree_sitter::Node, source: &[u8], ivy_code: &mut String) {
    let mut decls_count = 0;
    let mut vars_count = 0;
    let mut consts_count = 0;
    let mut elements_code = String::new();
    let mut interpolations_code = String::new();
    let mut consts_array = Vec::new();

    // Traverse the AST nodes and accumulate counts, generated Ivy code, and consts array
    self.traverse_ast(&node, source, &mut decls_count, &mut vars_count, &mut consts_count, &mut elements_code, &mut interpolations_code, &mut consts_array);

    // Generate the final Ivy code using the accumulated counts, generated code, and consts array
    *ivy_code += &format!(
        "static {{ this.ɵcmp = /*@__PURE__*/ i0.ɵɵdefineComponent({{ type: HomeComponent, selectors: [[\"app-home\"]], standalone: true, features: [i0.ɵɵStandaloneFeature], decls: {}, vars: {}, consts: {:?}, template: function HomeComponent_Template(rf, ctx) {{\n{}\n{}\n}} }}); }}",
        decls_count, vars_count, consts_array, elements_code, interpolations_code
    );
}

fn traverse_ast(&mut self, node: &tree_sitter::Node, source: &[u8], decls_count: &mut u32, vars_count: &mut u32, consts_count: &mut u32, elements_code: &mut String, interpolations_code: &mut String, consts_array: &mut Vec<Vec<String>>) {
    match node.kind() {
        "fragment" => {
            // Traverse child nodes recursively
            for i in 0..node.child_count() {
                let child = node.child(i).unwrap();
                self.traverse_ast(&child, source, decls_count, vars_count, consts_count, elements_code, interpolations_code, consts_array);
            }
        }
        "element" | "start_tag" | "end_tag" => {
            // Generate Ivy code for the element
            self.generate_element_ivy_code(node, source, decls_count, vars_count, consts_array);

            // Traverse child nodes recursively
            for i in 0..node.child_count() {
                let child = node.child(i).unwrap();
                self.traverse_ast(&child, source, decls_count, vars_count, consts_count, elements_code, interpolations_code, consts_array);
            }
        }
        // Count constants
        "const" => {
            *consts_count += 1;
            // Extract const values and add them to the consts array
            let const_value = node.utf8_text(source).unwrap();
            consts_array.push(vec![const_value.to_string()]);
        }
        // Generate Ivy code for interpolations
        "interpolation" => {
            for i in 0..node.child_count() {
                let expression_node = node.child(i).unwrap();
                if expression_node.kind() == "expression" {
                    for j in 0..expression_node.child_count() {
                        let identifier_node = expression_node.child(j).unwrap();
                        if identifier_node.kind() == "identifier" {
                            let identifier = identifier_node.utf8_text(source).unwrap();
                            *interpolations_code += &format!("i0.ɵɵtextInterpolate({});\n", identifier);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn generate_element_ivy_code(&mut self, node: &tree_sitter::Node, source: &[u8], decls_count: &mut u32, vars_count: &mut u32, consts_array: &mut Vec<Vec<String>>) {
    // Increment decls count for each element
    *decls_count += 1;

    let mut element_consts = Vec::new();

    // Extract attributes and their values
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap(); 
        if child.kind() == "start_tag" {
            for j in 0..child.child_count() {
                let attribute = child.child(j).unwrap();
                if attribute.kind() == "attribute" {
                    let name_node = attribute.child(0).unwrap();
                    let value_node = attribute.child(1).unwrap();
                    let attribute_name = name_node.utf8_text(source).unwrap();
                    let attribute_value = value_node.utf8_text(source).unwrap();
                    element_consts.push(attribute_name.to_string());
                    element_consts.push(attribute_value.to_string());
                }
            }
        }
    }

    // Add the element's consts to the consts array
    if !element_consts.is_empty() {
        consts_array.push(element_consts);
    }

    // Increment vars count for each structural directive
    if node.utf8_text(source).unwrap() == "ng-template" {
        *vars_count += 1;
    }
}

    
    

    // fn generate_ivy_code(&mut self, node: tree_sitter::Node, source: &[u8], ivy_code: &mut String) {
    //     println!("Node Kind: {}", node.kind());
    //     // for i in 0..node.named_child_count() {
    //     //     let child = node.named_child(i).unwrap(); // unwrap is safe here assuming the AST is correct
    //     //     println!("Field: {}", child.kind());
    //     // }
    //     // for child in node.children(&mut node.walk()) {
    //     //     self.generate_ivy_code(child, source, ivy_code);
    //     // }
    //     match node.kind() {
    //         "fragment" => {
    //             // Traverse child nodes recursively
    //             for child in node.children(&mut node.walk()) {
    //                 self.generate_ivy_code(child, source, ivy_code);
    //             }
    //         }
    //         "element" | "start_tag" | "end_tag" => {
    //             // Iterate over named children
    //             for i in 0..node.named_child_count() {
    //                 let child = node.named_child(i).unwrap(); // Unwrap is safe here assuming the AST is correct
    //                 if child.kind() == "tag_name" {
    //                     let tag_name = child.utf8_text(source).unwrap();
    //                     if node.kind() == "start_tag" {
    //                         *ivy_code += &format!("i0.ɵɵelementStart(0, \"{}\")", tag_name);
    //                     } else if node.kind() == "end_tag" {
    //                         *ivy_code += &format!("i0.ɵɵelementEnd(); // End tag for {}", tag_name);
    //                     }
    //                     *ivy_code += ";\n";
    //                     break; // Break after processing the tag name
    //                 }
    //             }
        
    //             // Generate Ivy code for attributes
    //             if let Some(attributes_node) = node.child_by_field_name("attributes") {
    //                 for attribute_node in attributes_node.children(&mut attributes_node.walk()) {
    //                     if attribute_node.kind() == "attribute" {
    //                         if let (Some(name_node), Some(value_node)) =
    //                             (attribute_node.child_by_field_name("attribute_name"), attribute_node.child_by_field_name("quoted_attribute_value"))
    //                         {
    //                             let attribute_name = name_node.utf8_text(source).unwrap();
    //                             let attribute_value = value_node.utf8_text(source).unwrap();
    //                             *ivy_code += &format!("i0.ɵɵattribute(\"{}\", \"{}\")", attribute_name, attribute_value);
    //                             *ivy_code += ";\n";
    //                         }
    //                     }
    //                 }
    //             }
        
    //             // Traverse child nodes recursively
    //             for child in node.children(&mut node.walk()) {
    //                 self.generate_ivy_code(child, source, ivy_code);
    //             }
    //         }
    //         _ => {}
    //     }
        
        
    // }

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
        self.three_parser
            .set_language(tree_sitter_angular::language())
            .expect("Error loading Rust grammar");

        let replace_kinds: HashSet<&str> =
            ["call_expression", "identifier"].iter().cloned().collect();
        let replacement = "ctx.";

        let html = r#"
        <div>
            <a href="https://analogjs.org/" target="_blank">
                <img alt="Analog Logo" class="logo analog" src="/analog.svg" />
            </a>
        </div>

        <h2>Analog</h2>

        <h3>The fullstack meta-framework for Angular!</h3>

        <div class="card">
            <button type="button" (click)="increment()">Count {{ count }}</button>
        </div>

        <p class="read-the-docs">
            For guides on how to customize this project, visit the
            <a href="https://analogjs.org" target="_blank">Analog documentation</a>
        </p>
    "#;
        let tree = self.three_parser.parse(html, None).unwrap();
        let root_node = tree.root_node();
        // Print the whole tree (for demonstration purposes)
        println!("{}", root_node.to_sexp());
        println!("Root node kind: {:?}", root_node.kind());

        let mut ivy_code = String::new();

        self.generate_ivy_code(tree.root_node(), html.as_bytes(), &mut ivy_code);

        println!("IVY CODE: {}", ivy_code);

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
