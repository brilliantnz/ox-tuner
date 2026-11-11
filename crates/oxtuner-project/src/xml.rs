//! A small, raw-preserving XML reader for `.msq` files.
//!
//! `.msq` files are XML with a very regular shape, and the round-trip rule is
//! stricter than XML well-formedness: **anything the model does not touch must
//! be emitted verbatim** — tag spelling, attribute quoting, indentation,
//! comments and elements this build does not understand. That is why this
//! module keeps each element's opening and closing tag as written and exposes
//! only [`Element::set_text`] as a mutation.
//!
//! The parser is deliberately small, but not naive:
//!
//! - a start tag is scanned with quote awareness, so `>` inside an attribute
//!   value does not truncate it;
//! - nesting is limited ([`MAX_DEPTH`]) so a hostile file cannot exhaust the
//!   stack — every error is structured ([`XmlErrorKind`]) and nothing panics;
//! - `DOCTYPE` and processing instructions are rejected instead of expanded
//!   (no entity substitution, no external resources);
//! - entities in text and attributes are decoded; unmatched or unknown
//!   entities stay literal rather than disappearing;
//! - CDATA is preserved as raw text and still round-trips.
//!
//! This is not a general XML implementation: namespaces are compared as
//! literal `prefix:name` strings, and there is no schema, DTD or `XPath`.

use std::fmt;

/// Maximum element nesting. `.msq` is three levels deep; the limit leaves
/// room for unknown wrappers without allowing unbounded recursion.
pub const MAX_DEPTH: usize = 64;

/// Position and kind of an XML problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    /// 1-based source line.
    pub line: usize,
    /// 1-based column, in bytes from the line start.
    pub column: usize,
    /// What went wrong.
    pub kind: XmlErrorKind,
}

impl fmt::Display for XmlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "line {}, column {}: {}",
            self.line, self.column, self.kind
        )
    }
}

impl std::error::Error for XmlError {}

/// The structured reason a document was rejected.
#[expect(
    clippy::exhaustive_enums,
    reason = "the parser reports a closed set of structural XML errors"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlErrorKind {
    /// The document has no elements at all.
    MissingRoot,
    /// A second top-level element was found.
    MultipleRoots,
    /// Text outside the root element.
    StrayText,
    /// `<` that starts neither an element, a comment, nor CDATA.
    UnsupportedMarkup {
        /// The markup that was found (`!DOCTYPE`, `!ELEMENT`, …).
        found: String,
    },
    /// A processing instruction other than the XML declaration.
    ProcessingInstruction,
    /// An element without a name.
    MissingElementName,
    /// A start tag that never ends.
    UnterminatedStartTag,
    /// An attribute without a value or without quotes.
    MalformedAttribute {
        /// The attribute name, when it could be read.
        name: String,
    },
    /// A quoted attribute value that never ends.
    UnterminatedAttributeValue {
        /// The attribute name.
        name: String,
    },
    /// `<![CDATA[` without its `]]>` terminator.
    UnterminatedCdata,
    /// `<!--` without a `-->` terminator after its body, including a
    /// terminator that overlaps the opener (`<!-->`).
    UnterminatedComment,
    /// `<?xml` without a `?>` terminator after its body, including a
    /// terminator that overlaps the opener (`<?>`).
    UnterminatedDeclaration,
    /// A closing tag that never ends.
    UnterminatedCloseTag,
    /// A closing tag whose name does not match the open element.
    MismatchedCloseTag {
        /// The element that was open.
        open: String,
        /// The name written in the closing tag.
        close: String,
    },
    /// A closing tag with no open element.
    UnexpectedCloseTag {
        /// The name written in the closing tag.
        close: String,
    },
    /// The document nests deeper than [`MAX_DEPTH`].
    TooDeep {
        /// The configured limit.
        limit: usize,
    },
}

impl fmt::Display for XmlErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRoot => formatter.write_str("the document has no root element"),
            Self::MultipleRoots => {
                formatter.write_str("the document has more than one root element")
            }
            Self::StrayText => formatter.write_str("text outside the root element"),
            Self::UnsupportedMarkup { found } => {
                write!(formatter, "unsupported markup `{found}`")
            }
            Self::ProcessingInstruction => {
                formatter.write_str("processing instructions are not supported")
            }
            Self::MissingElementName => formatter.write_str("element without a name"),
            Self::UnterminatedStartTag => formatter.write_str("unterminated start tag"),
            Self::MalformedAttribute { name } => {
                write!(formatter, "attribute `{name}` is missing a quoted value")
            }
            Self::UnterminatedAttributeValue { name } => {
                write!(formatter, "unterminated value for attribute `{name}`")
            }
            Self::UnterminatedCdata => formatter.write_str("unterminated CDATA section"),
            Self::UnterminatedComment => formatter.write_str("unterminated comment"),
            Self::UnterminatedDeclaration => formatter.write_str("unterminated XML declaration"),
            Self::UnterminatedCloseTag => formatter.write_str("unterminated closing tag"),
            Self::MismatchedCloseTag { open, close } => {
                write!(formatter, "closing tag </{close}> does not match <{open}>")
            }
            Self::UnexpectedCloseTag { close } => {
                write!(formatter, "closing tag </{close}> has no open element")
            }
            Self::TooDeep { limit } => {
                write!(formatter, "elements nest deeper than {limit} levels")
            }
        }
    }
}

/// One attribute, with its value decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Attribute name as written.
    pub name: String,
    /// Value with entities decoded.
    pub value: String,
}

/// One node of the document tree.
#[expect(
    clippy::exhaustive_enums,
    reason = "the document tree has a fixed set of node kinds"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// An element.
    Element(Element),
    /// Text exactly as written (entities still encoded).
    Text(String),
    /// A comment, without `<!--` and `-->`.
    Comment(String),
    /// A CDATA section, without `<![CDATA[` and `]]>`.
    Cdata(String),
}

/// One element, preserving its source spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Element name (namespace prefix included).
    pub name: String,
    /// Attributes in source order.
    pub attrs: Vec<Attr>,
    /// Child nodes in source order.
    pub children: Vec<Node>,
    /// The opening tag exactly as written, including `<` and `>`.
    pub raw_open: String,
    /// The closing tag exactly as written; empty for self-closing elements.
    pub raw_close: String,
    /// Whether the element was written as `<name ... />`.
    pub self_closing: bool,
}

impl Element {
    /// Builds an element whose opening tag is generated from `attrs`.
    ///
    /// `raw_open` and `attrs` are filled together: a generated element must
    /// read like a parsed one, or the serializer and [`Self::attr`] disagree.
    pub fn with_attrs(name: &str, attrs: &[(&str, &str)], self_closing: bool) -> Self {
        let mut raw_open = format!("<{name}");
        let mut parsed = Vec::with_capacity(attrs.len());
        for (key, value) in attrs {
            raw_open.push(' ');
            raw_open.push_str(key);
            raw_open.push_str("=\"");
            raw_open.push_str(&escape_attribute(value));
            raw_open.push('"');
            parsed.push(Attr {
                name: (*key).to_owned(),
                value: (*value).to_owned(),
            });
        }
        let raw_close = if self_closing {
            raw_open.push_str("/>");
            String::new()
        } else {
            raw_open.push('>');
            format!("</{name}>")
        };
        Self {
            name: name.to_owned(),
            attrs: parsed,
            children: Vec::new(),
            raw_open,
            raw_close,
            self_closing,
        }
    }

    /// One attribute by exact name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.name == name)
            .map(|attr| attr.value.as_str())
    }

    /// Child elements in document order.
    pub fn child_elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// The first child element with this name.
    pub fn find_child(&self, name: &str) -> Option<&Element> {
        self.child_elements().find(|element| element.name == name)
    }

    /// Every child element with this name.
    pub fn find_children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.child_elements()
            .filter(move |element| element.name == name)
    }

    /// The element's text content, entities decoded.
    ///
    /// Child elements are not descended into: a `<constant>` has text only, and
    /// concatenating nested markup would make a value silently wrong.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for node in &self.children {
            match node {
                Node::Text(raw) => text.push_str(&unescape_text(raw)),
                Node::Cdata(raw) => text.push_str(raw),
                Node::Element(_) | Node::Comment(_) => {}
            }
        }
        text
    }

    /// The element's text with surrounding whitespace removed.
    pub fn trimmed_text(&self) -> String {
        self.text().trim().to_owned()
    }

    /// Converts a self-closing element to the open/close form before it gains
    /// content.
    ///
    /// The serializer emits a self-closing element as `<name …/>` and never
    /// looks at its children, so adding text or a child without opening the
    /// element first would silently drop the change. Every mutation calls this
    /// first; an element that is already open is left exactly as written.
    pub(crate) fn open(&mut self) {
        if !self.self_closing {
            return;
        }
        self.self_closing = false;
        // `raw_open` ends with `/>`; drop the slash and keep the `>`.
        if self.raw_open.ends_with("/>") {
            self.raw_open.truncate(self.raw_open.len() - 2);
            self.raw_open.push('>');
        }
        self.raw_close = format!("</{}>", self.name);
    }

    /// Replaces the element's text with `value`, escaping it for XML.
    ///
    /// Child elements are left alone, so an unknown wrapper inside a constant
    /// survives; text nodes are replaced by a single escaped node.
    pub fn set_text(&mut self, value: &str) {
        self.open();
        let escaped = escape_text(value);
        let mut replaced = false;
        self.children.retain_mut(|node| match node {
            Node::Text(_) | Node::Cdata(_) => {
                if replaced {
                    false
                } else {
                    *node = Node::Text(escaped.clone());
                    replaced = true;
                    true
                }
            }
            Node::Element(_) | Node::Comment(_) => true,
        });
        if !replaced {
            self.children.push(Node::Text(escaped));
        }
    }
}

/// A parsed document: the XML declaration plus its top-level nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Everything between `<?` and `?>` of the declaration, when present.
    pub declaration: Option<String>,
    /// Top-level nodes in source order: whitespace, comments and the root.
    pub nodes: Vec<Node>,
}

impl Document {
    /// Parses an XML document from decoded source text.
    ///
    /// # Errors
    ///
    /// Returns [`XmlError`] with the byte position of the first structural
    /// problem.
    pub fn parse(source: &str) -> Result<Self, XmlError> {
        Parser::new(source).parse_document()
    }

    /// The root element.
    pub fn root(&self) -> Option<&Element> {
        self.nodes.iter().find_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// The root element, mutably.
    pub fn root_mut(&mut self) -> Option<&mut Element> {
        self.nodes.iter_mut().find_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// Serializes the tree back to source text.
    ///
    /// Untouched nodes are emitted from their preserved source strings, so a
    /// document that was parsed and not modified serializes identically.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        if let Some(declaration) = &self.declaration {
            out.push_str("<?");
            out.push_str(declaration);
            out.push_str("?>");
        }
        for node in &self.nodes {
            serialize_node(node, &mut out);
        }
        out
    }
}

fn serialize_node(node: &Node, out: &mut String) {
    match node {
        Node::Text(raw) => out.push_str(raw),
        Node::Comment(raw) => {
            out.push_str("<!--");
            out.push_str(raw);
            out.push_str("-->");
        }
        Node::Cdata(raw) => {
            out.push_str("<![CDATA[");
            out.push_str(raw);
            out.push_str("]]>");
        }
        Node::Element(element) => {
            out.push_str(&element.raw_open);
            if element.self_closing {
                debug_assert!(
                    element.children.is_empty(),
                    "a self-closing <{}> cannot hold children; call Element::open first",
                    element.name
                );
            } else {
                for child in &element.children {
                    serialize_node(child, out);
                }
                out.push_str(&element.raw_close);
            }
        }
    }
}

/// Escapes text for use in element content.
///
/// Quotes are left alone: they only need escaping inside a quoted attribute
/// ([`escape_attribute`]), and a `.msq` string value such as `"MAP"` is much
/// more readable written as itself.
pub fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

/// Escapes a value for use inside a double-quoted attribute.
pub fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

/// Decodes the five named entities and numeric character references.
///
/// Unknown or truncated entities are kept as written: for a value the tune
/// model cannot represent, showing the source is better than guessing.
pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            break;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with('#') => decode_character_reference(entity),
            _ => None,
        };
        match decoded {
            Some(character) => out.push(character),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_character_reference(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    char::from_u32(code)
}

struct Parser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    fn error<T>(&self, kind: XmlErrorKind) -> Result<T, XmlError> {
        let consumed = &self.source[..self.pos.min(self.source.len())];
        let line = consumed.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = consumed
            .rfind('\n')
            .map_or(consumed.len() + 1, |newline| consumed.len() - newline);
        Err(XmlError { line, column, kind })
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(needle)
    }

    fn parse_document(mut self) -> Result<Document, XmlError> {
        let mut declaration = None;
        let mut nodes: Vec<Node> = Vec::new();
        let mut root_seen = false;

        loop {
            self.push_whitespace(&mut nodes);
            let Some(byte) = self.peek() else {
                break;
            };
            if byte != b'<' {
                return self.error(XmlErrorKind::StrayText);
            }
            if self.starts_with(b"<?") {
                if root_seen || declaration.is_some() {
                    return self.error(XmlErrorKind::ProcessingInstruction);
                }
                declaration = Some(self.parse_declaration()?);
            } else if self.starts_with(b"<!--") {
                nodes.push(Node::Comment(self.parse_comment()?));
            } else if self.starts_with(b"<![CDATA[") {
                return self.error(XmlErrorKind::StrayText);
            } else if self.starts_with(b"<!") {
                let found = self.source[self.pos..]
                    .trim_start_matches("<!")
                    .split(|character: char| character.is_whitespace() || character == '>')
                    .next()
                    .unwrap_or("");
                return self.error(XmlErrorKind::UnsupportedMarkup {
                    found: format!("!{found}"),
                });
            } else if self.starts_with(b"</") {
                let close = self.close_tag_name()?;
                return self.error(XmlErrorKind::UnexpectedCloseTag { close });
            } else {
                if root_seen {
                    return self.error(XmlErrorKind::MultipleRoots);
                }
                nodes.push(Node::Element(self.parse_element(0)?));
                root_seen = true;
            }
        }

        if !root_seen {
            return self.error(XmlErrorKind::MissingRoot);
        }
        Ok(Document { declaration, nodes })
    }

    /// Consumes whitespace and keeps it as a text node, so serialization is
    /// byte-identical.
    fn push_whitespace(&mut self, nodes: &mut Vec<Node>) {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b'<' || !byte.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        if self.pos > start {
            nodes.push(Node::Text(self.source[start..self.pos].to_owned()));
        }
    }

    fn parse_declaration(&mut self) -> Result<String, XmlError> {
        let start = self.pos + 2;
        let Some(offset) = self.source[start..].find("?>") else {
            self.pos = self.bytes.len();
            return self.error(XmlErrorKind::UnterminatedDeclaration);
        };
        let end = start + offset;

        let declaration = self.source[start..end].to_owned();
        self.pos = end + 2;
        Ok(declaration)
    }

    fn parse_comment(&mut self) -> Result<String, XmlError> {
        let start = self.pos + 4;
        let Some(offset) = self.source[start..].find("-->") else {
            self.pos = self.bytes.len();
            return self.error(XmlErrorKind::UnterminatedComment);
        };
        let end = start + offset;

        let comment = self.source[start..end].to_owned();
        self.pos = end + 3;
        Ok(comment)
    }

    /// Reads a closing tag's name without consuming it.
    fn close_tag_name(&self) -> Result<String, XmlError> {
        let rest = &self.source[self.pos + 2..];
        let Some(end) = rest.find('>') else {
            let parser = Self {
                source: self.source,
                bytes: self.bytes,
                pos: self.bytes.len(),
            };
            return parser.error(XmlErrorKind::UnterminatedCloseTag);
        };
        Ok(rest[..end].trim().to_owned())
    }

    fn parse_element(&mut self, depth: usize) -> Result<Element, XmlError> {
        if depth >= MAX_DEPTH {
            return self.error(XmlErrorKind::TooDeep { limit: MAX_DEPTH });
        }

        let start = self.pos;
        self.pos += 1; // `<`

        let name_start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() || byte == b'>' || byte == b'/' {
                break;
            }
            self.pos += 1;
        }
        let name = self.source[name_start..self.pos].to_owned();
        if name.is_empty() {
            return self.error(XmlErrorKind::MissingElementName);
        }

        let mut attrs = Vec::new();
        let self_closing;
        loop {
            self.skip_inline_whitespace();
            match self.peek() {
                None => {
                    self.pos = self.bytes.len();
                    return self.error(XmlErrorKind::UnterminatedStartTag);
                }
                Some(b'>') => {
                    self.pos += 1;
                    self_closing = false;
                    break;
                }
                Some(b'/') => {
                    if !self.starts_with(b"/>") {
                        return self.error(XmlErrorKind::UnterminatedStartTag);
                    }
                    self.pos += 2;
                    self_closing = true;
                    break;
                }
                Some(_) => attrs.push(self.parse_attribute()?),
            }
        }
        let raw_open = self.source[start..self.pos].to_owned();

        if self_closing {
            return Ok(Element {
                name,
                attrs,
                children: Vec::new(),
                raw_open,
                raw_close: String::new(),
                self_closing: true,
            });
        }

        let mut children = Vec::new();
        loop {
            match self.peek() {
                None => {
                    self.pos = self.bytes.len();
                    return self.error(XmlErrorKind::MismatchedCloseTag {
                        open: name,
                        close: String::new(),
                    });
                }
                Some(b'<') => {
                    if self.starts_with(b"<!--") {
                        children.push(Node::Comment(self.parse_comment()?));
                    } else if self.starts_with(b"<![CDATA[") {
                        children.push(Node::Cdata(self.parse_cdata()?));
                    } else if self.starts_with(b"</") {
                        let raw_close = self.parse_close_tag(&name)?;
                        return Ok(Element {
                            name,
                            attrs,
                            children,
                            raw_open,
                            raw_close,
                            self_closing: false,
                        });
                    } else if self.starts_with(b"<?") {
                        return self.error(XmlErrorKind::ProcessingInstruction);
                    } else if self.starts_with(b"<!") {
                        return self.error(XmlErrorKind::UnsupportedMarkup {
                            found: "!".to_owned(),
                        });
                    } else {
                        children.push(Node::Element(self.parse_element(depth + 1)?));
                    }
                }
                Some(_) => {
                    let text_start = self.pos;
                    while let Some(byte) = self.peek() {
                        if byte == b'<' {
                            break;
                        }
                        self.pos += 1;
                    }
                    children.push(Node::Text(self.source[text_start..self.pos].to_owned()));
                }
            }
        }
    }

    /// Reads a close tag for `open_name` and returns its raw text.
    fn parse_close_tag(&mut self, open_name: &str) -> Result<String, XmlError> {
        let close_start = self.pos;
        self.pos += 2;
        self.skip_inline_whitespace();

        let close_name_start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() || byte == b'>' {
                break;
            }
            self.pos += 1;
        }
        let close_name = self.source[close_name_start..self.pos].to_owned();
        self.skip_inline_whitespace();
        if self.peek() != Some(b'>') {
            return self.error(XmlErrorKind::UnterminatedCloseTag);
        }
        self.pos += 1;

        if close_name != open_name {
            return self.error(XmlErrorKind::MismatchedCloseTag {
                open: open_name.to_owned(),
                close: close_name,
            });
        }

        Ok(self.source[close_start..self.pos].to_owned())
    }

    fn parse_cdata(&mut self) -> Result<String, XmlError> {
        let start = self.pos + 9;
        let Some(offset) = self.source[self.pos..].find("]]>") else {
            self.pos = self.bytes.len();
            return self.error(XmlErrorKind::UnterminatedCdata);
        };
        let end = self.pos + offset;
        let cdata = self.source[start..end].to_owned();
        self.pos = end + 3;
        Ok(cdata)
    }

    fn skip_inline_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if !byte.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
    }

    fn parse_attribute(&mut self) -> Result<Attr, XmlError> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() || byte == b'=' {
                break;
            }
            if byte == b'>' || byte == b'/' {
                return self.error(XmlErrorKind::MalformedAttribute {
                    name: self.source[start..self.pos].to_owned(),
                });
            }
            self.pos += 1;
        }
        let name = self.source[start..self.pos].to_owned();
        if name.is_empty() {
            return self.error(XmlErrorKind::MalformedAttribute {
                name: String::new(),
            });
        }
        self.skip_inline_whitespace();
        if self.peek() != Some(b'=') {
            return self.error(XmlErrorKind::MalformedAttribute { name });
        }
        self.pos += 1;
        self.skip_inline_whitespace();

        let Some(quote @ (b'"' | b'\'')) = self.peek() else {
            return self.error(XmlErrorKind::MalformedAttribute { name });
        };
        self.pos += 1;
        let value_start = self.pos;
        loop {
            match self.peek() {
                None => {
                    self.pos = self.bytes.len();
                    return self.error(XmlErrorKind::UnterminatedAttributeValue { name });
                }
                Some(byte) if byte == quote => break,
                Some(_) => self.pos += 1,
            }
        }
        let raw_value = &self.source[value_start..self.pos];
        self.pos += 1;
        Ok(Attr {
            name,
            value: unescape_text(raw_value),
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;

    fn parse(source: &str) -> Document {
        Document::parse(source).expect("the fixture is valid")
    }

    #[test]
    fn an_unmodified_document_serializes_byte_for_byte() {
        let source = "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n\
                      <msq xmlns=\"http://www.msefi.com/:msq\">\n\
                      \t<!-- keep me -->\n\
                      \t<versionInfo signature=\"speeduino 202501\"/>\n\
                      \t<page><constant name=\"a\">1.0</constant></page>\n\
                      </msq>\n";
        assert_eq!(parse(source).serialize(), source);
    }

    #[test]
    fn text_and_attributes_are_decoded_and_escaped_on_write() {
        let mut document = parse("<r a=\"x &amp; y\"><v>a &lt; b &amp; c</v></r>");
        assert_eq!(document.root().unwrap().attr("a"), Some("x & y"));
        let value = document
            .root_mut()
            .unwrap()
            .children
            .iter_mut()
            .find_map(|node| match node {
                Node::Element(element) if element.name == "v" => Some(element),
                _ => None,
            })
            .unwrap();
        assert_eq!(value.text(), "a < b & c");
        value.set_text("x < y & z");
        assert_eq!(
            document.serialize(),
            "<r a=\"x &amp; y\"><v>x &lt; y &amp; z</v></r>"
        );
    }

    #[test]
    fn a_greater_than_inside_an_attribute_does_not_end_the_tag() {
        let document = parse("<r note=\"a > b\"><c/></r>");
        let root = document.root().unwrap();
        assert_eq!(root.attr("note"), Some("a > b"));
        assert_eq!(root.find_children("c").count(), 1);
    }

    #[test]
    fn cdata_is_text_but_round_trips_as_cdata() {
        let source = "<r><v><![CDATA[1 < 2 & 3]]></v></r>";
        let document = parse(source);
        assert_eq!(
            document.root().unwrap().find_child("v").unwrap().text(),
            "1 < 2 & 3"
        );
        assert_eq!(document.serialize(), source);
    }

    #[test]
    fn a_numeric_character_reference_is_decoded() {
        assert_eq!(unescape_text("a&#65;b&#x42;c"), "aAbBc");
        assert_eq!(unescape_text("&unknown; &"), "&unknown; &");
    }

    #[test]
    fn malformed_documents_are_structured_errors_not_panics() {
        let cases = [
            ("", XmlErrorKind::MissingRoot),
            ("   ", XmlErrorKind::MissingRoot),
            (
                "<a>",
                XmlErrorKind::MismatchedCloseTag {
                    open: "a".to_owned(),
                    close: String::new(),
                },
            ),
            (
                "<a></b>",
                XmlErrorKind::MismatchedCloseTag {
                    open: "a".to_owned(),
                    close: "b".to_owned(),
                },
            ),
            (
                "</a>",
                XmlErrorKind::UnexpectedCloseTag {
                    close: "a".to_owned(),
                },
            ),
            ("<a><b/></a><c/>", XmlErrorKind::MultipleRoots),
            ("text", XmlErrorKind::StrayText),
            (
                "<a x=1/>",
                XmlErrorKind::MalformedAttribute {
                    name: "x".to_owned(),
                },
            ),
            (
                "<a x=\"1/>",
                XmlErrorKind::UnterminatedAttributeValue {
                    name: "x".to_owned(),
                },
            ),
            ("<a><?pi ?></a>", XmlErrorKind::ProcessingInstruction),
            (
                "<!DOCTYPE msq><a/>",
                XmlErrorKind::UnsupportedMarkup {
                    found: "!DOCTYPE".to_owned(),
                },
            ),
            ("<?>", XmlErrorKind::UnterminatedDeclaration),
            ("<!-->", XmlErrorKind::UnterminatedComment),
            ("<!--->", XmlErrorKind::UnterminatedComment),
            ("<a><!-- x</a>", XmlErrorKind::UnterminatedComment),
            ("<a><![CDATA[x</a>", XmlErrorKind::UnterminatedCdata),
        ];
        for (source, expected) in cases {
            let error = Document::parse(source).expect_err(source);
            assert_eq!(error.kind, expected, "{source}");
        }
    }

    #[test]
    fn nesting_deeper_than_the_limit_is_rejected() {
        let mut source = String::new();
        for _ in 0..=MAX_DEPTH {
            source.push_str("<a>");
        }
        for _ in 0..=MAX_DEPTH {
            source.push_str("</a>");
        }
        let error = Document::parse(&source).expect_err("too deep");
        assert_eq!(error.kind, XmlErrorKind::TooDeep { limit: MAX_DEPTH });
    }

    #[test]
    fn editing_a_self_closing_element_opens_it_instead_of_dropping_the_text() {
        let mut document = parse("<r><v a=\"1\"/></r>");
        let value = document
            .root_mut()
            .unwrap()
            .children
            .iter_mut()
            .find_map(|node| match node {
                Node::Element(element) if element.name == "v" => Some(element),
                _ => None,
            })
            .unwrap();

        value.set_text("x < y");

        assert_eq!(
            document.serialize(),
            "<r><v a=\"1\">x &lt; y</v></r>",
            "the value must be written, not swallowed by `<v/>`"
        );
    }

    #[test]
    fn an_untouched_self_closing_element_keeps_its_form() {
        let source = "<r><v a=\"1\"/></r>";
        assert_eq!(parse(source).serialize(), source);
    }

    #[test]
    fn an_error_reports_line_and_column() {
        let error = Document::parse("<a>\n  <b x=1/>\n</a>").expect_err("malformed");
        assert_eq!(error.line, 2);
        assert!(error.column > 1);
        assert!(error.to_string().starts_with("line 2, column"));
    }
}
