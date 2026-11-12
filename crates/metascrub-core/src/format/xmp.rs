//! XMP: RDF/XML packets found in JPEG APP1, PNG `iTXt` and Photoshop resources.
//!
//! Properties are identified by namespace URI, not prefix, because prefixes
//! are chosen freely by each writer.

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const NAMESPACES: &[(&str, &str)] = &[
    ("http://purl.org/dc/elements/1.1/", "dc"),
    ("http://ns.adobe.com/xap/1.0/", "xmp"),
    ("http://ns.adobe.com/xap/1.0/mm/", "xmpMM"),
    ("http://ns.adobe.com/xap/1.0/sType/ResourceEvent#", "stEvt"),
    ("http://ns.adobe.com/xap/1.0/sType/ResourceRef#", "stRef"),
    ("http://ns.adobe.com/xap/1.0/rights/", "xmpRights"),
    ("http://ns.adobe.com/xap/1.0/g/img/", "xmpGImg"),
    ("http://ns.adobe.com/photoshop/1.0/", "photoshop"),
    ("http://ns.adobe.com/exif/1.0/", "exif"),
    ("http://cipa.jp/exif/1.0/", "exifEX"),
    ("http://ns.adobe.com/exif/1.0/aux/", "aux"),
    ("http://ns.adobe.com/tiff/1.0/", "tiff"),
    (
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
        "Iptc4xmpCore",
    ),
    ("http://iptc.org/std/Iptc4xmpExt/2008-02-29/", "Iptc4xmpExt"),
    ("http://ns.adobe.com/camera-raw-settings/1.0/", "crs"),
    ("http://ns.google.com/photos/1.0/camera/", "GCamera"),
    ("http://ns.google.com/photos/1.0/container/", "Container"),
    ("http://ns.google.com/photos/1.0/container/item/", "Item"),
    (GAIN_MAP, "hdrgm"),
];

/// Ultra HDR's gain map settings (Adobe's gain map namespace).
pub(super) const GAIN_MAP: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";

/// The namespaces an Ultra HDR photo needs: the gain map settings and the
/// directory that says where the gain map is.
pub(super) const ULTRA_HDR: &[&str] = &["hdrgm", "Container", "Item"];

/// Namespaces that only give the packet its structure.
const STRUCTURAL: &[&str] = &[
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "adobe:ns:meta/",
    "http://www.w3.org/XML/1998/namespace",
];

fn classify(prefix: &str, name: &str) -> Option<FindingKind> {
    use FindingKind::*;
    Some(match (prefix, name) {
        ("dc", "creator" | "rights" | "contributor" | "publisher") => PersonName,
        ("dc", "title" | "description" | "subject") => Description,
        ("dc", "date") => Timestamp,
        ("xmp", "CreatorTool") => Software,
        ("xmp", "CreateDate" | "ModifyDate" | "MetadataDate") => Timestamp,
        ("xmpMM", "DocumentID" | "InstanceID" | "OriginalDocumentID") => DocumentId,
        ("stEvt", "softwareAgent") => Software,
        ("stEvt", "when") => Timestamp,
        ("stEvt", "instanceID") => DocumentId,
        ("stRef", "documentID" | "instanceID" | "originalDocumentID") => DocumentId,
        ("stRef", "filePath") => FilePath,
        ("xmpRights", "Owner") => PersonName,
        ("xmpRights", "UsageTerms" | "WebStatement") => Description,
        ("xmpGImg", "image") => Thumbnail,
        ("photoshop", "City" | "State" | "Country") => GpsLocation,
        ("photoshop", "Credit" | "AuthorsPosition" | "CaptionWriter") => PersonName,
        ("photoshop", "Headline" | "Instructions" | "Source") => Description,
        ("photoshop", "DateCreated") => Timestamp,
        ("photoshop", "DocumentAncestors") => DocumentId,
        ("exif", name) if name.starts_with("GPS") => GpsLocation,
        ("exif", "DateTimeOriginal" | "DateTimeDigitized") => Timestamp,
        ("exif", "UserComment") => Comment,
        ("exif" | "exifEX" | "aux", "BodySerialNumber" | "LensSerialNumber" | "SerialNumber") => {
            DeviceSerial
        }
        ("exifEX", "CameraOwnerName") | ("aux", "OwnerName") => PersonName,
        ("exifEX", "LensMake" | "LensModel") | ("aux", "Lens" | "LensInfo" | "LensID") => {
            DeviceModel
        }
        ("tiff", "Make" | "Model") => DeviceModel,
        ("tiff", "Artist" | "Copyright") => PersonName,
        ("tiff", "Software") => Software,
        ("tiff", "DateTime") => Timestamp,
        ("tiff", "ImageDescription") => Description,
        ("Iptc4xmpCore", "Location") => GpsLocation,
        ("Iptc4xmpCore", name) if name.starts_with("Ci") => PersonName,
        (
            "Iptc4xmpExt",
            "City" | "CountryName" | "CountryCode" | "ProvinceState" | "Sublocation"
            | "WorldRegion",
        ) => GpsLocation,
        ("Iptc4xmpExt", "PersonInImage") => PersonName,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
struct Property {
    /// `None` for a namespace MetaScrub doesn't know.
    prefix: Option<&'static str>,
    name: String,
}

#[derive(Debug)]
struct Element {
    /// `None` for structural elements (`rdf:Description`, `rdf:li`, …).
    property: Option<Property>,
    text: String,
}

#[derive(Debug, Default)]
struct Collector {
    /// Prefixes of namespaces that aren't reported.
    skip: &'static [&'static str],
    findings: Vec<Finding>,
    technical: i64,
    unknown: i64,
}

impl Collector {
    fn record(&mut self, property: &Property, value: &str, location: &Location) {
        let Some(prefix) = property.prefix else {
            self.unknown += 1;
            return;
        };
        if self.skip.contains(&prefix) {
            return;
        }
        let Some(kind) = classify(prefix, &property.name) else {
            self.technical += 1;
            return;
        };
        let location = location.child(format!("{prefix}:{}", property.name));
        let value = if kind == FindingKind::Thumbnail {
            Value::Bytes(value.len() as u64)
        } else {
            Value::Text(value.to_owned())
        };
        // Lists (several creators, keywords) become one finding.
        match self.findings.iter_mut().find(|f| f.location == location) {
            Some(Finding {
                value: Some(Value::Text(existing)),
                ..
            }) => {
                if let Value::Text(more) = value {
                    existing.push_str(", ");
                    existing.push_str(&more);
                }
            }
            _ => self
                .findings
                .push(Finding::new(kind, location, Disposition::Remove).with_value(value)),
        }
    }

    fn finish(mut self, location: &Location) -> Vec<Finding> {
        for (kind, count) in [
            (FindingKind::TechnicalInfo, self.technical),
            (FindingKind::UnknownMetadata, self.unknown),
        ] {
            if count > 0 {
                self.findings.push(
                    Finding::new(kind, location.clone(), Disposition::Remove)
                        .with_value(Value::Integer(count)),
                );
            }
        }
        self.findings
    }
}

/// An XMP packet with nothing in it, padded with spaces to exactly `len`
/// bytes (packets are padded so they can be edited in place). `None` when
/// it doesn't fit.
pub(super) fn empty_packet(len: usize) -> Option<Vec<u8>> {
    const HEAD: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
        <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
        xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/></x:xmpmeta>";
    const TAIL: &str = "<?xpacket end=\"w\"?>";
    let padding = len.checked_sub(HEAD.len() + TAIL.len())?;
    Some([HEAD.as_bytes(), &vec![b' '; padding], TAIL.as_bytes()].concat())
}

pub(super) fn inspect(data: &[u8], location: &Location) -> Vec<Finding> {
    inspect_except(data, location, &[])
}

/// Like [`inspect`], but properties in the namespaces with the prefixes in
/// `skip` aren't reported.
pub(super) fn inspect_except(
    data: &[u8],
    location: &Location,
    skip: &'static [&'static str],
) -> Vec<Finding> {
    let mut collector = Collector {
        skip,
        ..Collector::default()
    };
    if parse(data, location, &mut collector).is_err() {
        collector.findings.push(
            Finding::new(
                FindingKind::UnknownMetadata,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(data.len() as u64)),
        );
    }
    collector.finish(location)
}

fn parse(
    data: &[u8],
    location: &Location,
    collector: &mut Collector,
) -> Result<(), quick_xml::Error> {
    let mut reader = NsReader::from_reader(data);
    let mut stack: Vec<Element> = Vec::new();
    loop {
        let (ns, event) = reader.read_resolved_event()?;
        let property = |name: &BytesStart<'_>| -> Option<Property> {
            namespace(&ns).map(|prefix| Property {
                prefix,
                name: AsRef::<str>::as_ref(&name.local_name()).to_owned(),
            })
        };
        match event {
            Event::Start(element) => {
                let property = property(&element);
                attributes(&reader, &element, location, collector)?;
                stack.push(Element {
                    property,
                    text: String::new(),
                });
            }
            Event::Empty(element) => {
                attributes(&reader, &element, location, collector)?;
            }
            Event::Text(text) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&text.xml10_content());
                }
            }
            Event::CData(text) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&text.xml10_content());
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(top) = stack.last_mut() {
                    let c = match reference.resolve_char_ref()? {
                        Some(c) => Some(c),
                        None => match &*reference {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        },
                    };
                    top.text.extend(c);
                }
            }
            Event::End(_) => {
                let Some(element) = stack.pop() else { continue };
                let text = element.text.trim();
                if text.is_empty() {
                    continue;
                }
                // Text inside rdf:li belongs to the nearest enclosing property.
                let owner = element
                    .property
                    .as_ref()
                    .or_else(|| stack.iter().rev().find_map(|e| e.property.as_ref()));
                if let Some(owner) = owner {
                    collector.record(&owner.clone(), text, location);
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Simple properties written as attributes: `<rdf:Description dc:format="…">`.
fn attributes(
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
    location: &Location,
    collector: &mut Collector,
) -> Result<(), quick_xml::Error> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(quick_xml::Error::from)?;
        if attribute.key.as_namespace_binding().is_some() {
            continue;
        }
        let (ns, name) = reader.resolver().resolve_attribute(attribute.key);
        let Some(prefix) = namespace(&ns) else {
            continue;
        };
        let Some(prefix) = prefix else {
            // Unknown namespace: count it without a value.
            collector.unknown += 1;
            continue;
        };
        let value = attribute.normalized_value(XmlVersion::Implicit1_0)?;
        let property = Property {
            prefix: Some(prefix),
            name: AsRef::<str>::as_ref(&name).to_owned(),
        };
        collector.record(&property, value.trim(), location);
    }
    Ok(())
}

/// `None`: structural or unbound (not a property).
/// `Some(None)`: a property in a namespace MetaScrub doesn't know.
fn namespace(ns: &ResolveResult<'_>) -> Option<Option<&'static str>> {
    let ResolveResult::Bound(ns) = ns else {
        return None;
    };
    let uri: &str = ns.as_ref();
    if STRUCTURAL.contains(&uri) {
        return None;
    }
    Some(
        NAMESPACES
            .iter()
            .find(|(known, _)| *known == uri)
            .map(|(_, prefix)| *prefix),
    )
}

/// A gain map setting: its name and value, or values for each color channel.
pub(super) type Setting = (String, Vec<String>);

/// The gain map settings (`hdrgm:*`) in a packet, in order, written either
/// as attributes or as elements (with an `rdf:Seq` for one value per
/// channel). `None` when the packet can't be read.
pub(super) fn gain_map_settings(data: &[u8]) -> Option<Vec<Setting>> {
    let mut reader = NsReader::from_reader(data);
    let mut settings: Vec<Setting> = Vec::new();
    // The setting being read as an element, and how deep inside it we are.
    let mut open: Option<(String, Vec<String>, usize)> = None;
    let is_gain_map = |ns: &ResolveResult<'_>| matches!(ns, ResolveResult::Bound(ns) if AsRef::<str>::as_ref(ns) == GAIN_MAP);
    loop {
        let (ns, event) = reader.read_resolved_event().ok()?;
        let in_gain_map = is_gain_map(&ns);
        match &event {
            Event::Start(element) | Event::Empty(element) => {
                for attribute in element.attributes() {
                    let attribute = attribute.ok()?;
                    if attribute.key.as_namespace_binding().is_some() {
                        continue;
                    }
                    let (ns, name) = reader.resolver().resolve_attribute(attribute.key);
                    if is_gain_map(&ns) {
                        let value = attribute.normalized_value(XmlVersion::Implicit1_0).ok()?;
                        let name: &str = name.as_ref();
                        settings.push((name.to_owned(), vec![value.trim().to_owned()]));
                    }
                }
                let empty = matches!(event, Event::Empty(_));
                match &mut open {
                    Some((_, _, depth)) if !empty => *depth += 1,
                    None if in_gain_map && !empty => {
                        let name = element.local_name();
                        let name: &str = name.as_ref();
                        open = Some((name.to_owned(), Vec::new(), 0));
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if let Some((_, values, _)) = &mut open {
                    let text = text.xml10_content();
                    if !text.trim().is_empty() {
                        values.push(text.trim().to_owned());
                    }
                }
            }
            Event::End(_) => match open.take() {
                Some((name, values, 0)) => settings.push((name, values)),
                Some((name, values, depth)) => open = Some((name, values, depth - 1)),
                None => {}
            },
            Event::Eof => return Some(settings),
            _ => {}
        }
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A packet with only `settings` and, for the primary image of an Ultra HDR
/// photo, the directory listing it and its gain map of `gain_map_len` bytes.
pub(super) fn gain_map_packet(settings: &[Setting], gain_map_len: Option<usize>) -> Vec<u8> {
    let mut out = String::from(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\"",
    );
    if gain_map_len.is_some() {
        out.push_str(
            " xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" \
             xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\"",
        );
    }
    for (name, values) in settings {
        if let [value] = values.as_slice() {
            out.push_str(&format!(" hdrgm:{name}=\"{}\"", escape(value)));
        }
    }
    out.push('>');
    for (name, values) in settings {
        if values.len() > 1 {
            out.push_str(&format!("<hdrgm:{name}><rdf:Seq>"));
            for value in values {
                out.push_str(&format!("<rdf:li>{}</rdf:li>", escape(value)));
            }
            out.push_str(&format!("</rdf:Seq></hdrgm:{name}>"));
        }
    }
    if let Some(len) = gain_map_len {
        out.push_str(&format!(
            "<Container:Directory><rdf:Seq>\
             <rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"Primary\" Item:Mime=\"image/jpeg\"/></rdf:li>\
             <rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"GainMap\" Item:Mime=\"image/jpeg\" Item:Length=\"{len}\"/></rdf:li>\
             </rdf:Seq></Container:Directory>"
        ));
    }
    out.push_str("</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:ps="http://ns.adobe.com/photoshop/1.0/"
    xmlns:stRef="http://ns.adobe.com/xap/1.0/sType/ResourceRef#"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:vendor="http://example.com/private/"
    xmp:CreatorTool="Editor 1.0"
    xmp:Rating="3"
    vendor:Secret="abc">
   <dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li><rdf:li>John Roe</rdf:li></rdf:Seq></dc:creator>
   <ps:City>Paris &amp; suburbs</ps:City>
   <xmpMM:DerivedFrom stRef:filePath="C:\Users\jane\photo.psd"/>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    fn find(findings: &[Finding], location: &str) -> Option<Value> {
        findings
            .iter()
            .find(|f| f.location.to_string() == location)
            .and_then(|f| f.value.clone())
    }

    #[test]
    fn finds_properties_by_namespace_not_prefix() {
        let findings = inspect(PACKET.as_bytes(), &Location::new(["XMP"]));
        assert_eq!(
            find(&findings, "XMP > xmp:CreatorTool"),
            Some(Value::Text("Editor 1.0".into()))
        );
        assert_eq!(
            find(&findings, "XMP > dc:creator"),
            Some(Value::Text("Jane Doe, John Roe".into()))
        );
        // Written with the prefix "ps", reported with the standard "photoshop".
        assert_eq!(
            find(&findings, "XMP > photoshop:City"),
            Some(Value::Text("Paris & suburbs".into()))
        );
        assert_eq!(
            find(&findings, r"XMP > stRef:filePath"),
            Some(Value::Text(r"C:\Users\jane\photo.psd".into()))
        );
    }

    #[test]
    fn counts_technical_and_unknown_properties() {
        let findings = inspect(PACKET.as_bytes(), &Location::new(["XMP"]));
        let count = |kind| {
            findings
                .iter()
                .find(|f| f.kind == kind)
                .and_then(|f| f.value.clone())
        };
        assert_eq!(count(FindingKind::TechnicalInfo), Some(Value::Integer(1)));
        assert_eq!(count(FindingKind::UnknownMetadata), Some(Value::Integer(1)));
    }

    #[test]
    fn malformed_xml_is_reported() {
        let findings = inspect(b"<a><b></a>", &Location::new(["XMP"]));
        assert!(
            findings
                .iter()
                .any(|f| f.kind == FindingKind::UnknownMetadata)
        );
    }

    #[test]
    fn gain_map_settings_survive_a_rewrite() {
        let packet = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 9.1"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" hdrgm:Version="1.0" hdrgm:GainMapMax="2.3" xmp:CreatorTool="Editor"><hdrgm:Gamma><rdf:Seq><rdf:li>1</rdf:li><rdf:li>1.1</rdf:li><rdf:li>0.9</rdf:li></rdf:Seq></hdrgm:Gamma></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let settings = gain_map_settings(packet).unwrap();
        assert_eq!(
            settings,
            [
                ("Version".to_owned(), vec!["1.0".to_owned()]),
                ("GainMapMax".to_owned(), vec!["2.3".to_owned()]),
                (
                    "Gamma".to_owned(),
                    vec!["1".to_owned(), "1.1".to_owned(), "0.9".to_owned()]
                ),
            ]
        );
        let rewritten = gain_map_packet(&settings, Some(1234));
        assert_eq!(gain_map_settings(&rewritten).unwrap(), settings);
        assert!(inspect_except(&rewritten, &Location::new(["XMP"]), ULTRA_HDR).is_empty());
        // The original reports the editor but not the settings.
        let findings = inspect_except(packet, &Location::new(["XMP"]), ULTRA_HDR);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Software);
    }
}
