//! A small XML tree for rewriting Office parts. Whatever isn't changed is
//! written back exactly as it was read.

use std::borrow::Cow;

use quick_xml::NsReader;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{Writer, XmlVersion};

use crate::format::FormatError;

#[derive(Debug, Clone)]
pub(super) enum Node {
    Element(Element),
    Other(Event<'static>),
}

#[derive(Debug, Clone)]
pub(super) struct Element {
    start: BytesStart<'static>,
    end: Option<BytesEnd<'static>>,
    pub(super) ns: String,
    pub(super) local: String,
    attributes: Vec<Attr>,
    pub(super) children: Vec<Node>,
    /// Set when attributes were removed, so the start tag must be rebuilt.
    changed: bool,
}

#[derive(Debug, Clone)]
struct Attr {
    ns: String,
    local: String,
    key: String,
    /// As written in the file, still escaped.
    raw: String,
    value: String,
}

pub(super) fn parse(data: &[u8]) -> Result<Vec<Node>, FormatError> {
    parse_xml(data).map_err(|e| FormatError::malformed(format!("XML: {e}")))
}

fn parse_xml(data: &[u8]) -> Result<Vec<Node>, quick_xml::Error> {
    let mut reader = NsReader::from_reader(data);
    let mut root = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    loop {
        let (ns, event) = reader.read_resolved_event()?;
        let ns = namespace(&ns);
        match event {
            Event::Start(start) => stack.push(element(&reader, ns, start)?),
            Event::Empty(start) => {
                let element = element(&reader, ns, start)?;
                push(&mut stack, &mut root, Node::Element(element));
            }
            Event::End(end) => {
                let Some(mut element) = stack.pop() else {
                    continue;
                };
                element.end = Some(end.into_owned());
                push(&mut stack, &mut root, Node::Element(element));
            }
            Event::Eof => break,
            other => push(&mut stack, &mut root, Node::Other(other.into_owned())),
        }
    }
    if !stack.is_empty() {
        return Err(quick_xml::Error::Syntax(
            quick_xml::errors::SyntaxError::UnclosedTag,
        ));
    }
    Ok(root)
}

fn push(stack: &mut [Element], root: &mut Vec<Node>, node: Node) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(node),
        None => root.push(node),
    }
}

fn namespace(ns: &ResolveResult<'_>) -> String {
    match ns {
        ResolveResult::Bound(ns) => AsRef::<str>::as_ref(ns).to_owned(),
        _ => String::new(),
    }
}

fn element(
    reader: &NsReader<&[u8]>,
    ns: String,
    start: BytesStart<'_>,
) -> Result<Element, quick_xml::Error> {
    let local = AsRef::<str>::as_ref(&start.local_name()).to_owned();
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute?;
        let (attribute_ns, attribute_local) = reader.resolver().resolve_attribute(attribute.key);
        attributes.push(Attr {
            ns: namespace(&attribute_ns),
            local: AsRef::<str>::as_ref(&attribute_local).to_owned(),
            key: attribute.key.0.to_owned(),
            raw: attribute.value.clone().into_owned(),
            value: attribute
                .normalized_value(XmlVersion::Implicit1_0)?
                .into_owned(),
        });
    }
    Ok(Element {
        start: start.into_owned(),
        end: None,
        ns,
        local,
        attributes,
        children: Vec::new(),
        changed: false,
    })
}

impl Element {
    pub(super) fn is(&self, ns: &[&str], local: &str) -> bool {
        self.local == local && ns.contains(&self.ns.as_str())
    }

    pub(super) fn attribute(&self, ns: &[&str], local: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.local == local && ns.contains(&a.ns.as_str()))
            .map(|a| a.value.as_str())
    }

    /// The first descendant (or this element) matching the name.
    pub(super) fn find(&self, ns: &[&str], local: &str) -> Option<&Element> {
        if self.is(ns, local) {
            return Some(self);
        }
        self.elements().find_map(|child| child.find(ns, local))
    }

    pub(super) fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Other(_) => None,
        })
    }

    /// Removes the attributes `remove` matches; returns how many.
    pub(super) fn remove_attributes(&mut self, remove: impl Fn(&str, &str, &str) -> bool) -> usize {
        let before = self.attributes.len();
        self.attributes
            .retain(|a| !remove(&a.ns, &a.local, &a.value));
        let removed = before - self.attributes.len();
        self.changed |= removed > 0;
        removed
    }

    /// All text inside the element, with entities resolved.
    pub(super) fn text(&self) -> String {
        let mut text = String::new();
        for node in &self.children {
            match node {
                Node::Element(element) => text.push_str(&element.text()),
                Node::Other(Event::Text(t)) => text.push_str(&t.xml10_content()),
                Node::Other(Event::CData(t)) => text.push_str(&t.xml10_content()),
                Node::Other(Event::GeneralRef(r)) => {
                    let c = match r.resolve_char_ref().ok().flatten() {
                        Some(c) => Some(c),
                        None => match &**r {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        },
                    };
                    text.extend(c);
                }
                Node::Other(_) => {}
            }
        }
        text
    }
}

/// Visits every element, depth first, letting `visit` change it.
pub(super) fn for_each_element(nodes: &mut [Node], visit: &mut impl FnMut(&mut Element)) {
    for node in nodes {
        if let Node::Element(element) = node {
            visit(element);
            for_each_element(&mut element.children, visit);
        }
    }
}

/// Removes every element `remove` matches, with everything inside it.
/// Returns the removed elements.
pub(super) fn remove_elements(
    nodes: &mut Vec<Node>,
    remove: &mut impl FnMut(&Element) -> bool,
) -> Vec<Element> {
    let mut removed = Vec::new();
    let mut kept = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        match node {
            Node::Element(element) if remove(&element) => removed.push(element),
            Node::Element(mut element) => {
                removed.extend(remove_elements(&mut element.children, remove));
                kept.push(Node::Element(element));
            }
            other => kept.push(other),
        }
    }
    *nodes = kept;
    removed
}

pub(super) fn write(nodes: &[Node]) -> Result<Vec<u8>, FormatError> {
    let mut writer = Writer::new(Vec::new());
    write_nodes(&mut writer, nodes)?;
    Ok(writer.into_inner())
}

fn write_nodes(writer: &mut Writer<Vec<u8>>, nodes: &[Node]) -> Result<(), FormatError> {
    for node in nodes {
        match node {
            Node::Other(event) => writer.write_event(event.borrow())?,
            Node::Element(element) => {
                let start = element.start_tag();
                match &element.end {
                    None => writer.write_event(Event::Empty(start))?,
                    Some(end) => {
                        writer.write_event(Event::Start(start))?;
                        write_nodes(writer, &element.children)?;
                        writer.write_event(Event::End(end.borrow()))?;
                    }
                }
            }
        }
    }
    Ok(())
}

impl Element {
    fn start_tag(&self) -> BytesStart<'_> {
        if !self.changed {
            return self.start.borrow();
        }
        let mut start = self.start.borrow();
        start.clear_attributes();
        for attribute in &self.attributes {
            // Values are written between double quotes.
            let raw = if attribute.raw.contains('"') {
                Cow::Owned(attribute.raw.replace('"', "&quot;"))
            } else {
                Cow::Borrowed(attribute.raw.as_str())
            };
            start.push_attribute(Attribute {
                key: QName(&attribute.key),
                value: raw,
            });
        }
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &str = "urn:w";

    #[test]
    fn unchanged_xml_is_written_back_byte_for_byte() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
                   <w:doc xmlns:w=\"urn:w\" a='1 &amp; 2'><!-- note --><w:p>A &lt; B</w:p>\
                   <w:e/><![CDATA[x]]></w:doc>";
        let nodes = parse(xml.as_bytes()).unwrap();
        assert_eq!(write(&nodes).unwrap(), xml.as_bytes());
    }

    #[test]
    fn removes_attributes_and_elements() {
        let xml = "<w:doc xmlns:w=\"urn:w\"><w:p w:rsidR=\"00A1\" w:x=\"1\">t</w:p><w:rsids><w:rsid/></w:rsids></w:doc>";
        let mut nodes = parse(xml.as_bytes()).unwrap();
        let mut count = 0;
        for_each_element(&mut nodes, &mut |e| {
            count += e.remove_attributes(|ns, local, _| ns == W && local.starts_with("rsid"));
        });
        let removed = remove_elements(&mut nodes, &mut |e| e.is(&[W], "rsids"));
        assert_eq!(count, 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(
            String::from_utf8(write(&nodes).unwrap()).unwrap(),
            "<w:doc xmlns:w=\"urn:w\"><w:p w:x=\"1\">t</w:p></w:doc>"
        );
    }

    #[test]
    fn reads_text_and_attributes() {
        let nodes =
            parse(b"<a xmlns:w=\"urn:w\" w:v=\"x &amp; y\">Jane &amp; <b>John</b></a>").unwrap();
        let Some(Node::Element(a)) = nodes.first() else {
            panic!("no element");
        };
        assert_eq!(a.text(), "Jane & John");
        assert_eq!(a.attribute(&[W], "v"), Some("x & y"));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"<a>").is_err());
    }
}
