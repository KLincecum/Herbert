//! Lossless XML tree for native DataSet file builders.
use herbert_contract::Diagnostic;
use quick_xml::{
    Reader, Writer,
    events::{BytesEnd, BytesStart, BytesText, Event},
};
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("NATIVE_XML_INVALID", message)
}
#[derive(Clone, Debug)]
pub(crate) struct Element {
    pub(crate) name: String,
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) content: Vec<Content>,
}
#[derive(Clone, Debug)]
pub(crate) enum Content {
    Element(Element),
    Text(String),
}
impl Element {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attributes: vec![],
            content: vec![],
        }
    }
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.0 == name)
            .map(|a| a.1.as_str())
    }
    pub(crate) fn set(&mut self, name: &str, value: &str) {
        self.attributes.retain(|a| a.0 != name);
        self.attributes.push((name.into(), value.into()));
    }
    pub(crate) fn children(&self) -> impl DoubleEndedIterator<Item = &Element> {
        self.content.iter().filter_map(|c| {
            if let Content::Element(e) = c {
                Some(e)
            } else {
                None
            }
        })
    }
    pub(crate) fn child_mut(&mut self, local: &str) -> Option<&mut Element> {
        self.content.iter_mut().find_map(|c| {
            if let Content::Element(e) = c {
                if e.name.rsplit(':').next() == Some(local) {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub(crate) fn push(&mut self, child: Element) {
        self.content.push(Content::Element(child));
    }
    pub(crate) fn write(&self, writer: &mut Writer<Vec<u8>>) -> Result<(), Diagnostic> {
        let mut start = BytesStart::new(&self.name);
        // Attribute entities must survive XML whitespace normalization.
        for (key, value) in &self.attributes {
            let value = crate::classic_archive::escape(value)?
                .replace('\n', "&#10;")
                .replace('\t', "&#9;");
            start.push_attribute((key.as_bytes(), value.as_bytes()));
        }
        writer
            .write_event(Event::Start(start))
            .map_err(|_| invalid("Cannot write the DataSet XML."))?;
        for child in &self.content {
            match child {
                Content::Element(e) => e.write(writer)?,
                Content::Text(t) => writer
                    .write_event(Event::Text(BytesText::from_escaped(
                        crate::classic_archive::escape(t)?,
                    )))
                    .map_err(|_| invalid("Cannot write the DataSet text."))?,
            }
        }
        writer
            .write_event(Event::End(BytesEnd::new(&self.name)))
            .map_err(|_| invalid("Cannot finish the DataSet XML."))
    }
}
pub(crate) fn parse(xml: &str) -> Result<Element, Diagnostic> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = Vec::<Element>::new();
    let mut root = None;
    loop {
        match reader
            .read_event()
            .map_err(|_| invalid("DataSet XML is malformed XML."))?
        {
            Event::Start(e) => {
                let mut node = Element::new(
                    std::str::from_utf8(e.name().as_ref())
                        .map_err(|_| invalid("An XML name is invalid."))?,
                );
                for attribute in e.attributes() {
                    let a = attribute.map_err(|_| invalid("An XML attribute is invalid."))?;
                    node.set(
                        std::str::from_utf8(a.key.as_ref())
                            .map_err(|_| invalid("An XML attribute name is invalid."))?,
                        &a.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )
                        .map_err(|_| invalid("An XML attribute value is invalid."))?,
                    );
                }
                stack.push(node);
            }
            Event::End(_) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| invalid("An XML element has no start."))?;
                if let Some(parent) = stack.last_mut() {
                    parent.push(node);
                } else if root.replace(node).is_some() {
                    return Err(invalid("DataSet XML has multiple roots."));
                }
            }
            Event::Text(e) => {
                let decoded = e.decode().map_err(|_| invalid("XML text is invalid."))?;
                if let Some(node) = stack.last_mut() {
                    node.content.push(Content::Text(decoded.into_owned()));
                } else if !decoded.trim().is_empty() {
                    return Err(invalid("XML text is outside its root."));
                }
            }
            Event::CData(e) => {
                let text = e.decode().map_err(|_| invalid("XML CDATA is invalid."))?;
                stack
                    .last_mut()
                    .ok_or_else(|| invalid("CDATA is outside its root."))?
                    .content
                    .push(Content::Text(text.into_owned()));
            }
            Event::GeneralRef(e) => {
                let name = e
                    .decode()
                    .map_err(|_| invalid("An XML reference is invalid."))?;
                let escaped = format!("&{name};");
                let text = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| invalid("An XML entity is unresolved."))?;
                stack
                    .last_mut()
                    .ok_or_else(|| invalid("An XML reference is outside its root."))?
                    .content
                    .push(Content::Text(text.into_owned()));
            }
            Event::DocType(_) => {
                return Err(invalid("DataSet XML cannot contain a document type."));
            }
            Event::Eof => break,
            _ => (),
        }
    }
    if !stack.is_empty() {
        return Err(invalid("DataSet XML is incomplete."));
    }
    root.ok_or_else(|| invalid("DataSet XML is empty."))
}
