//! A forgiving reader for the XML nvhttp answers with: elements, attributes,
//! text. Hosts differ (GFE, Sunshine, Apollo, us) in whitespace, declarations
//! and what they nest, so this keeps only what clients look up: a tree of
//! named elements. Bounded in depth and size, and never panics on any input.

/// Most elements read from one document; more are dropped.
const MAX_NODES: usize = 20_000;
/// Elements nested deeper than this are flattened into their parent.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// The text directly inside, entities resolved, trimmed.
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first descendant (depth first) called `name`.
    pub fn find(&self, name: &str) -> Option<&Node> {
        for child in &self.children {
            if child.name == name {
                return Some(child);
            }
            if let Some(found) = child.find(name) {
                return Some(found);
            }
        }
        None
    }

    /// The text of the first descendant called `name`.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.find(name).map(|n| n.text.as_str())
    }

    /// The direct children called `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// Parses `input` into a document node whose children are the top-level elements.
pub(super) fn parse(input: &str) -> Node {
    // `stack[0]` is the document; the rest are the open elements.
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut nodes = 0usize;
    let mut rest = input;
    let mut text = String::new();

    fn close(stack: &mut Vec<Node>, text: &mut String) {
        if let Some(top) = stack.last_mut() {
            top.text.push_str(text.trim());
        }
        text.clear();
        if stack.len() > 1
            && let Some(done) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            parent.children.push(done);
        }
    }

    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            text.push_str(&unescape(rest));
            break;
        };
        text.push_str(&unescape(&rest[..lt]));
        rest = &rest[lt..];
        if let Some(body) = rest.strip_prefix("<!--") {
            rest = body.find("-->").map_or("", |i| &body[i + 3..]);
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let end = body.find("]]>").unwrap_or(body.len());
            text.push_str(&body[..end]);
            rest = body.get(end + 3..).unwrap_or("");
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
        } else {
            let Some(gt) = tag_end(rest) else {
                break;
            };
            let tag = &rest[1..gt];
            rest = &rest[gt + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                let name = name.trim();
                // Closes the nearest open element of that name, and whatever
                // was left open inside it; a stray closing tag is ignored.
                if let Some(depth) = stack.iter().rposition(|n| n.name == name)
                    && depth > 0
                {
                    while stack.len() > depth {
                        close(&mut stack, &mut text);
                    }
                }
                continue;
            }
            let self_closing = tag.ends_with('/');
            let tag = tag.strip_suffix('/').unwrap_or(tag);
            let (name, attrs) = split_tag(tag);
            if name.is_empty() || nodes >= MAX_NODES {
                continue;
            }
            nodes += 1;
            // Text before a child belongs to the parent.
            if let Some(top) = stack.last_mut() {
                top.text.push_str(text.trim());
                text.clear();
            }
            let node = Node {
                name: name.to_owned(),
                attrs,
                ..Node::default()
            };
            if self_closing || stack.len() > MAX_DEPTH {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            } else {
                stack.push(node);
            }
        }
    }
    while stack.len() > 1 {
        close(&mut stack, &mut text);
    }
    stack.pop().unwrap_or_default()
}

/// The index of the `>` that ends the tag starting at `s[0] == '<'`, skipping
/// quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, b) in s.bytes().enumerate().skip(1) {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), b) if b == q => quote = None,
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

fn split_tag(tag: &str) -> (&str, Vec<(String, String)>) {
    let tag = tag.trim();
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let (name, mut rest) = tag.split_at(name_end);
    let mut attrs = Vec::new();
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim();
        let value_part = rest[eq + 1..].trim_start();
        let (value, after) = match value_part.chars().next() {
            Some(q @ ('"' | '\'')) => match value_part[1..].find(q) {
                Some(end) => (&value_part[1..1 + end], &value_part[end + 2..]),
                None => (&value_part[1..], ""),
            },
            _ => {
                let end = value_part
                    .find(char::is_whitespace)
                    .unwrap_or(value_part.len());
                (&value_part[..end], &value_part[end..])
            }
        };
        if !key.is_empty() && attrs.len() < 64 {
            attrs.push((key.to_owned(), unescape(value)));
        }
        rest = after;
    }
    (name, attrs)
}

/// Resolves the five named entities and numeric references; anything else is kept.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let entity = &rest[1..semi];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity.strip_prefix('#').and_then(|n| {
                    match n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse().ok(),
                    }
                    .and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_host_builds() {
        let doc = parse(
            "<?xml version=\"1.0\"?><root status_code=\"200\" status_message=\"a &amp; b\"><hostname>Cha &lt;1&gt;</hostname>\
             <App><AppTitle>Steam &amp; Co</AppTitle><ID>2</ID></App><App><AppTitle>X</AppTitle><ID>3</ID></App><empty/></root>",
        );
        let root = doc.find("root").unwrap();
        assert_eq!(root.attr("status_code"), Some("200"));
        assert_eq!(root.attr("status_message"), Some("a & b"));
        assert_eq!(root.text_of("hostname"), Some("Cha <1>"));
        let apps: Vec<_> = root
            .children_named("App")
            .map(|a| (a.text_of("AppTitle"), a.text_of("ID")))
            .collect();
        assert_eq!(
            apps,
            [(Some("Steam & Co"), Some("2")), (Some("X"), Some("3"))]
        );
        assert_eq!(root.text_of("empty"), Some(""));
        assert!(root.text_of("missing").is_none());
    }

    #[test]
    fn tolerates_what_hosts_get_wrong() {
        // Unclosed elements, a stray closing tag, comments, CDATA, numeric entities, single quotes.
        let doc = parse(
            "<root status_code='200'><!-- c --><a>1<b>2</c></b></a><x><![CDATA[<raw>]]></x><y>&#65;&#x42;&nope;</y>",
        );
        let root = doc.find("root").unwrap();
        assert_eq!(root.attr("status_code"), Some("200"));
        assert_eq!(root.text_of("b"), Some("2"));
        assert_eq!(root.text_of("x"), Some("<raw>"));
        assert_eq!(root.text_of("y"), Some("AB&nope;"));
        assert_eq!(parse("").children.len(), 0);
        assert_eq!(parse("just text").children.len(), 0);
    }

    #[test]
    fn depth_and_size_are_bounded() {
        let deep = "<a>".repeat(10_000);
        let doc = parse(&deep);
        assert!(doc.find("a").is_some());
        let wide = "<a/>".repeat(MAX_NODES * 2);
        let doc = parse(&wide);
        assert!(doc.children.len() <= MAX_NODES);
    }

    /// Whatever bytes arrive, the parser returns.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let atoms = [
            "<",
            ">",
            "</",
            "/>",
            "<root ",
            "status_code=",
            "\"",
            "'",
            "&",
            "&#",
            ";",
            "<!--",
            "-->",
            "<![CDATA[",
            "]]>",
            "<?",
            "?>",
            "=",
            " ",
            "é",
            "\u{0}",
            "a",
            "App",
        ];
        for _ in 0..30_000 {
            let mut s = String::new();
            for _ in 0..(next() % 24) {
                s.push_str(atoms[(next() % atoms.len() as u64) as usize]);
            }
            let _ = parse(&s);
        }
        for _ in 0..2_000 {
            let bytes: Vec<u8> = (0..next() % 200).map(|_| next() as u8).collect();
            let _ = parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
