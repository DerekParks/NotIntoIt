//! Field descriptions (`<toolTip>`, `<speak>`, captions) from an XFA template, keyed by AcroForm
//! full name: named containers contribute `name[i]`, unnamed subforms are transparent.

use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;

const NAMED_CONTAINERS: &[&str] = &["subform", "exclGroup", "field"];
const SKIPPED: &[&str] = &[
    "pageSet",
    "proto",
    "variables",
    "script",
    "event",
    "calculate",
    "validate",
];

struct Frame {
    tag: String,
    /// `name[i]` when this element is a named SOM node.
    component: Option<String>,
    counters: HashMap<String, u32>,
}

#[derive(Default)]
struct FieldText {
    tooltip: String,
    speak: String,
    caption: String,
}

impl FieldText {
    fn sink(&mut self, stack: &[Frame]) -> Option<&mut String> {
        let within = |tag: &str| stack.iter().any(|f| f.tag == tag);
        if within("toolTip") {
            Some(&mut self.tooltip)
        } else if within("speak") {
            Some(&mut self.speak)
        } else if within("caption") {
            Some(&mut self.caption)
        } else {
            None
        }
    }
}

/// AcroForm full field name -> description, for every field with any text.
pub fn descriptions(template_xml: &[u8]) -> Result<HashMap<String, String>> {
    let mut reader = Reader::from_reader(template_xml);
    let mut buf = Vec::new();
    let mut stack: Vec<Frame> = vec![Frame {
        tag: String::new(),
        component: None,
        counters: HashMap::new(),
    }];
    let mut skip_depth: Option<usize> = None;
    let mut field: Option<(usize, String, FieldText)> = None;
    let mut out = HashMap::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let frame = open(&mut stack, &e, skip_depth.is_some());
                let tag = frame.tag.clone();
                let component = frame.component.clone();
                stack.push(frame);
                if skip_depth.is_none() && SKIPPED.contains(&tag.as_str()) {
                    skip_depth = Some(stack.len());
                }
                if skip_depth.is_none() && tag == "field" && field.is_none() && component.is_some() {
                    field = Some((stack.len(), som_path(&stack), FieldText::default()));
                }
            }
            Event::Empty(e) => {
                open(&mut stack, &e, skip_depth.is_some());
            }
            Event::End(_) => {
                if skip_depth == Some(stack.len()) {
                    skip_depth = None;
                }
                if field.as_ref().is_some_and(|(depth, _, _)| *depth == stack.len()) {
                    let (_, name, text) = field.take().expect("checked above");
                    let description = [text.tooltip, text.speak, text.caption]
                        .into_iter()
                        .map(|t| normalize_ws(&t))
                        .find(|t| !t.is_empty());
                    if let Some(d) = description {
                        out.insert(name, d);
                    }
                } else if let Some(sink) = field.as_mut().and_then(|(_, _, t)| t.sink(&stack)) {
                    sink.push(' ');
                }
                stack.pop();
            }
            Event::Text(t) if skip_depth.is_none() => {
                if let Some(sink) = field.as_mut().and_then(|(_, _, text)| text.sink(&stack)) {
                    sink.push_str(&t.xml10_content()?);
                }
            }
            Event::GeneralRef(r) if skip_depth.is_none() => {
                if let (Some(sink), Some(c)) = (
                    field.as_mut().and_then(|(_, _, text)| text.sink(&stack)),
                    r.resolve_char_ref()?,
                ) {
                    sink.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Build the frame for an opening element, bumping its SOM index in the
/// nearest named scope.
fn open(stack: &mut [Frame], e: &BytesStart, skipping: bool) -> Frame {
    let tag = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
    let name = e.try_get_attribute("name").ok().flatten().and_then(|a| {
        a.normalized_value(quick_xml::XmlVersion::Explicit1_0)
            .ok()
            .map(|v| v.into_owned())
    });
    let component = match name {
        Some(name) if !skipping && NAMED_CONTAINERS.contains(&tag.as_str()) => {
            let scope = stack
                .iter_mut()
                .rev()
                .find(|f| f.component.is_some() || f.tag.is_empty())
                .expect("root frame is always a scope");
            let counter = scope.counters.entry(name.clone()).or_default();
            let index = *counter;
            *counter += 1;
            Some(format!("{name}[{index}]"))
        }
        _ => None,
    };
    Frame {
        tag,
        component,
        counters: HashMap::new(),
    }
}

fn som_path(stack: &[Frame]) -> String {
    stack
        .iter()
        .filter_map(|f| f.component.as_deref())
        .collect::<Vec<_>>()
        .join(".")
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"<template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
 <subform name="topmostSubform">
  <pageSet><pageArea name="Page1"><field name="hdr"><assist><speak>master</speak></assist></field></pageArea></pageSet>
  <subform name="Page1">
   <field name="f1_01"><assist><speak disable="1">Your first name &#x2014; and initial. </speak></assist></field>
   <subform>
    <field name="c1_8"><assist><speak>Filing Status. Single.</speak></assist></field>
   </subform>
   <subform name="Checkbox_ReadOrder">
    <field name="c1_8"><assist><toolTip>Married filing jointly</toolTip><speak>ignored</speak></assist></field>
    <draw name="label"><value><text>not a field</text></value></draw>
    <field name="c1_8"><caption><value><text>Head of</text></value></caption><caption><value><text>household</text></value></caption></field>
   </subform>
   <field name="f1_02"/>
  </subform>
 </subform>
</template>"#;

    #[test]
    fn som_names_match_acroform_naming() {
        let d = descriptions(TEMPLATE.as_bytes()).unwrap();
        assert_eq!(
            d["topmostSubform[0].Page1[0].f1_01[0]"],
            "Your first name — and initial."
        );
        // Unnamed subform is transparent: this is c1_8[0] directly under Page1.
        assert_eq!(d["topmostSubform[0].Page1[0].c1_8[0]"], "Filing Status. Single.");
        assert_eq!(
            d["topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0]"],
            "Married filing jointly"
        );
        assert_eq!(
            d["topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[1]"],
            "Head of household"
        );
        assert!(!d.contains_key("topmostSubform[0].Page1[0].f1_02[0]"));
        assert!(
            d.keys().all(|k| !k.contains("hdr")),
            "pageSet fields must be skipped: {d:?}"
        );
    }
}
