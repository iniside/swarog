//! A strict HTML tree for the portal templates and the mockup's template-literal output.
//! Both sides are generated, well-formed markup, so a stray or missing close tag is an
//! error rather than an implied close: a misparse would show up as a false difference.

use anyhow::{bail, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    /// Raw source text, entities undecoded.
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// Lowercase.
    pub tag: String,
    /// Values with entities decoded.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

const VOID: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title"];

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|c| c == class)
    }

    /// The direct text children, decoded and whitespace-collapsed; nested elements' text is
    /// left out.
    pub fn own_text(&self) -> String {
        let raw: Vec<&str> = self
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect();
        collapse(&decode(&raw.join(" ")))
    }

    /// All descendant text, decoded and whitespace-collapsed.
    pub fn text(&self) -> String {
        let mut raw = String::new();
        collect_text(&self.children, &mut raw);
        collapse(&decode(&raw))
    }

    /// The first descendant (not `self`) matching `pred`, in document order.
    pub fn find(&self, pred: &dyn Fn(&Element) -> bool) -> Option<&Element> {
        find_in(&self.children, pred)
    }
}

fn collect_text(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) => {
                out.push(' ');
                out.push_str(t);
            }
            Node::Element(e) if e.tag != "script" && e.tag != "style" => collect_text(&e.children, out),
            Node::Element(_) => {}
        }
    }
}

pub fn find_in<'a>(nodes: &'a [Node], pred: &dyn Fn(&Element) -> bool) -> Option<&'a Element> {
    for n in nodes {
        if let Node::Element(e) = n {
            if pred(e) {
                return Some(e);
            }
            if let Some(found) = find_in(&e.children, pred) {
                return Some(found);
            }
        }
    }
    None
}

/// Every element matching `pred`, in document order, descending into matches too.
pub fn find_all<'a>(nodes: &'a [Node], pred: &dyn Fn(&Element) -> bool, out: &mut Vec<&'a Element>) {
    for n in nodes {
        if let Node::Element(e) = n {
            if pred(e) {
                out.push(e);
            }
            find_all(&e.children, pred, out);
        }
    }
}

pub fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| name.strip_prefix('#').map(|dec| dec.parse::<u32>()))
                    .and_then(|r| r.ok())
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses a document or fragment into its top-level nodes. Comments, doctypes and
/// processing instructions are dropped.
pub fn parse(src: &str) -> Result<Vec<Node>> {
    let mut stack: Vec<Element> = vec![Element { tag: String::new(), attrs: Vec::new(), children: Vec::new() }];
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let end = src[i..].find('<').map_or(src.len(), |n| i + n);
            push(&mut stack, Node::Text(src[i..end].to_string()));
            i = end;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else { bail!("unterminated comment at byte {i}") };
            i += end + 3;
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            let Some(end) = rest.find('>') else { bail!("unterminated declaration at byte {i}") };
            i += end + 1;
        } else if let Some(after) = rest.strip_prefix("</") {
            let Some(end) = after.find('>') else { bail!("unterminated close tag at byte {i}") };
            let name = after[..end].trim().to_ascii_lowercase();
            let top = stack.last().map(|e| e.tag.clone()).unwrap_or_default();
            if stack.len() == 1 {
                bail!("stray </{name}> at byte {i}");
            }
            if top != name {
                bail!("</{name}> at byte {i} closes <{top}>, which is still open");
            }
            let done = stack.pop().expect("stack holds the root and the closed element");
            push(&mut stack, Node::Element(done));
            i += 2 + end + 1;
        } else if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            let (element, self_closed, len) = open_tag(rest).map_err(|e| anyhow::anyhow!("{e} at byte {i}"))?;
            i += len;
            if self_closed || VOID.contains(&element.tag.as_str()) {
                push(&mut stack, Node::Element(element));
            } else if RAW_TEXT.contains(&element.tag.as_str()) {
                let close = format!("</{}", element.tag);
                let Some(end) = src[i..].to_ascii_lowercase().find(&close) else {
                    bail!("unterminated <{}> at byte {i}", element.tag)
                };
                let mut element = element;
                if end > 0 {
                    element.children.push(Node::Text(src[i..i + end].to_string()));
                }
                let Some(gt) = src[i + end..].find('>') else { bail!("unterminated </{}> at byte {i}", element.tag) };
                i += end + gt + 1;
                push(&mut stack, Node::Element(element));
            } else {
                stack.push(element);
            }
        } else {
            push(&mut stack, Node::Text("<".to_string()));
            i += 1;
        }
    }
    if stack.len() > 1 {
        let open: Vec<String> = stack[1..].iter().map(|e| format!("<{}>", e.tag)).collect();
        bail!("unclosed {} at end of input", open.join(" "));
    }
    Ok(stack.pop().map(|root| root.children).unwrap_or_default())
}

fn push(stack: &mut [Element], node: Node) {
    stack.last_mut().expect("the root never pops").children.push(node);
}

/// The element of the open tag at the start of `src`, whether it ended in `/>`, and the
/// tag's byte length.
fn open_tag(src: &str) -> Result<(Element, bool, usize)> {
    let bytes = src.as_bytes();
    let mut i = 1;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/' {
        i += 1;
    }
    let tag = src[1..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            bail!("unterminated <{tag}>");
        }
        match bytes[i] {
            b'>' => return Ok((Element { tag, attrs, children: Vec::new() }, false, i + 1)),
            b'/' if bytes.get(i + 1) == Some(&b'>') => {
                return Ok((Element { tag, attrs, children: Vec::new() }, true, i + 2))
            }
            b'/' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let name = src[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            attrs.push((name, String::new()));
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let Some(len) = src[i + 1..].find(q as char) else { bail!("unterminated attribute {name} in <{tag}>") };
                let v = decode(&src[i + 1..i + 1 + len]);
                i += len + 2;
                v
            }
            _ => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                decode(&src[start..i])
            }
        };
        attrs.push((name, value));
    }
}

/// The nodes as HTML again: double-quoted attributes, void elements without a close tag.
pub fn serialize(nodes: &[Node]) -> String {
    let mut out = String::new();
    write_nodes(nodes, &mut out);
    out
}

fn write_nodes(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) => {
                out.push('<');
                out.push_str(&e.tag);
                for (k, v) in &e.attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&v.replace('&', "&amp;").replace('"', "&quot;"));
                    out.push('"');
                }
                out.push('>');
                if !VOID.contains(&e.tag.as_str()) {
                    write_nodes(&e.children, out);
                    out.push_str("</");
                    out.push_str(&e.tag);
                    out.push('>');
                }
            }
        }
    }
}
