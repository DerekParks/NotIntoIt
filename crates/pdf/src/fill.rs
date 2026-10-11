//! Form filling via lopdf: text values get generated appearance streams, checkbox/radio
//! states use each widget's own on-state; pdfium flattens.

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object, ObjectId};
use pdfium_render::prelude::*;
use std::collections::{BTreeMap, HashMap};

use crate::acroform::{self, FieldKind, FieldNode};
use crate::appearance::{TextStyle, set_text_appearance};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Bool(bool),
}

/// Fill `values` (field name, or any unique dotted suffix of one -> value) into `pdf`,
/// returning the new PDF. All values are validated before anything is written.
pub fn fill(pdf: &[u8], values: &BTreeMap<String, Value>) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(pdf).context("parsing PDF")?;
    let nodes: HashMap<String, FieldNode> = acroform::collect(&doc)?
        .into_iter()
        .map(|n| (n.field.name.clone(), n))
        .collect();

    let Plan { texts, buttons } = plan(&nodes, values)?;

    let default_da = acroform::default_da(&doc);
    for (node, text) in &texts {
        write_text(&mut doc, node, text, default_da.as_deref())?;
    }
    for (node, state) in &buttons {
        acroform::set_button_state(&mut doc, node, state)?;
    }
    acroform::strip_xfa(&mut doc)?;
    acroform::save(&mut doc)
}

/// Burn form values into page content and drop the form, as a printed copy would be.
pub fn flatten(pdfium: &Pdfium, pdf: &[u8]) -> Result<Vec<u8>> {
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    for mut page in document.pages().iter() {
        page.flatten()?;
    }
    let mut doc = Document::load_mem(&document.save_to_bytes()?).context("parsing pdfium output")?;
    doc.catalog_mut()?.remove(b"AcroForm");
    acroform::save(&mut doc)
}

/// An exact field name, or the one field whose name ends with `.{name}`.
fn resolve_name<'a>(nodes: &'a HashMap<String, FieldNode>, name: &str) -> Result<&'a FieldNode> {
    if let Some(node) = nodes.get(name) {
        return Ok(node);
    }
    let suffix = format!(".{name}");
    let mut matches: Vec<&FieldNode> = nodes.values().filter(|n| n.field.name.ends_with(&suffix)).collect();
    match matches.len() {
        0 => {
            let leaf = name.rsplit('.').next().unwrap_or(name);
            let mut similar: Vec<&str> = nodes
                .values()
                .map(|n| n.field.name.as_str())
                .filter(|full| full.rsplit('.').next() == Some(leaf))
                .collect();
            similar.sort_unstable();
            match similar.as_slice() {
                [] => bail!("no such field"),
                _ => bail!("no such field; did you mean {}?", similar.join(" or ")),
            }
        }
        1 => Ok(matches.remove(0)),
        _ => {
            let mut names: Vec<&str> = matches.iter().map(|n| n.field.name.as_str()).collect();
            names.sort_unstable();
            bail!("ambiguous; matches {}", names.join(", "))
        }
    }
}

/// What `fill` will write, once every value has been checked.
struct Plan<'a> {
    texts: Vec<(&'a FieldNode, &'a str)>,
    buttons: Vec<(&'a FieldNode, Vec<u8>)>,
}

/// Match each value to its field and check it fits, reporting every problem at once.
fn plan<'a>(nodes: &'a HashMap<String, FieldNode>, values: &'a BTreeMap<String, Value>) -> Result<Plan<'a>> {
    let mut texts: Vec<(&FieldNode, &str)> = Vec::new();
    let mut buttons: Vec<(&FieldNode, Vec<u8>)> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut claimed: HashMap<&str, &str> = HashMap::new();
    for (name, value) in values {
        let node = match resolve_name(nodes, name) {
            Ok(node) => node,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        if let Some(other) = claimed.insert(node.field.name.as_str(), name) {
            problems.push(format!("{name}: same field as {other}"));
            continue;
        }
        match resolve(&node.field.kind, value) {
            Ok(Resolved::Text) => {
                let Value::Text(s) = value else {
                    unreachable!("resolve checked the variant")
                };
                texts.push((node, s));
            }
            Ok(Resolved::Button(state)) => buttons.push((node, state)),
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    if !problems.is_empty() {
        bail!("nothing written; fix these fields:\n  {}", problems.join("\n  "));
    }
    Ok(Plan { texts, buttons })
}

/// Set a text field's `/V` and draw its value on every widget.
fn write_text(doc: &mut Document, node: &FieldNode, text: &str, default_da: Option<&str>) -> Result<()> {
    doc.get_object_mut(node.id)?
        .as_dict_mut()?
        .set("V", lopdf::text_string(text));
    for widget in &node.widgets {
        let style = widget_style(doc, widget.id, node, default_da)?;
        set_text_appearance(doc, widget.id, &style, text).with_context(|| format!("drawing {}", node.field.name))?;
    }
    Ok(())
}

/// A widget's text style: its own `/DA` and `/Q`, else the field's, else the form's default `/DA`.
fn widget_style(doc: &Document, widget: ObjectId, node: &FieldNode, default_da: Option<&str>) -> Result<TextStyle> {
    let own = doc.get_dictionary(widget)?;
    let own_da = own.get(b"DA").ok().and_then(|d| lopdf::decode_text_string(d).ok());
    Ok(TextStyle {
        da: own_da
            .or(node.da.clone())
            .or(default_da.map(String::from))
            .context("text field has no /DA")?,
        quadding: own.get(b"Q").and_then(Object::as_i64).unwrap_or(node.quadding),
        flags: node.flags,
        max_len: node.max_len,
    })
}

enum Resolved {
    Text,
    Button(Vec<u8>),
}

fn resolve(kind: &FieldKind, value: &Value) -> Result<Resolved> {
    match (kind, value) {
        (FieldKind::Text { max_len, .. }, Value::Text(s)) => {
            if let Some(max) = max_len {
                let len = s.chars().count();
                if len > *max as usize {
                    let digits = s.chars().filter(char::is_ascii_alphanumeric).count();
                    let hint = if digits <= *max as usize {
                        "; enter it without dashes or spaces"
                    } else {
                        ""
                    };
                    bail!("{len} characters exceeds this field's limit of {max}{hint}");
                }
            }
            Ok(Resolved::Text)
        }
        (FieldKind::Text { .. }, Value::Bool(_)) => bail!("text field needs a string value"),
        (FieldKind::Checkbox { on_states } | FieldKind::Radio { states: on_states }, value) => {
            let state = match value {
                Value::Bool(false) => "Off",
                Value::Bool(true) => match on_states.as_slice() {
                    [only] => only.as_str(),
                    _ => bail!("has several on-states {on_states:?}; pass one of them as a string"),
                },
                Value::Text(s) if s == "Off" || on_states.contains(s) => s.as_str(),
                Value::Text(s) => {
                    bail!("{s:?} is not a state of this box; use true/false or one of {on_states:?}")
                }
            };
            Ok(Resolved::Button(state.as_bytes().to_vec()))
        }
        (FieldKind::Other { pdf_type }, _) => bail!("{pdf_type} fields are not fillable yet"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acroform::{normalize, read_fields};

    const F1040: &[u8] = include_bytes!("../tests/fixtures/f1040--2025.pdf");
    const SINGLE: &str = "topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0]";
    const MFJ: &str = "topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[1]";
    const FIRST_NAME: &str = "topmostSubform[0].Page1[0].f1_14[0]";
    const SSN: &str = "topmostSubform[0].Page1[0].f1_16[0]";
    const RENDER_WIDTH: u32 = 1275;

    fn pdfium() -> &'static Pdfium {
        crate::engine::pdfium(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data"))
            .expect("pdfium install")
    }

    fn values(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// Mean darkness (0 = white, 255 = black) of a field's first widget in the flattened PDF,
    /// so only stored appearance streams count, never pdfium's own regeneration.
    fn ink(pdf: &[u8], field: &str) -> f64 {
        let doc = Document::load_mem(pdf).unwrap();
        let node = acroform::collect(&doc)
            .unwrap()
            .into_iter()
            .find(|n| n.field.name == field)
            .unwrap();
        let widget = &node.widgets[0];
        let page_no = widget.page.unwrap();
        let [x1, y1, x2, y2] = numbers(&doc, doc.get_dictionary(widget.id).unwrap(), b"Rect");
        let media = numbers(
            &doc,
            doc.get_dictionary(doc.get_pages()[&page_no]).unwrap(),
            b"MediaBox",
        );
        let flat = flatten(pdfium(), pdf).unwrap();
        let png = crate::render::render_page(pdfium(), &flat, page_no, RENDER_WIDTH)
            .unwrap()
            .png;
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        let scale = img.width() as f32 / (media[2] - media[0]);
        let (px1, px2) = (((x1 - media[0]) * scale) as u32, ((x2 - media[0]) * scale) as u32);
        let (py1, py2) = (((media[3] - y2) * scale) as u32, ((media[3] - y1) * scale) as u32);
        let mut total = 0u64;
        let mut n = 0u64;
        for y in py1..py2.min(img.height()) {
            for x in px1..px2.min(img.width()) {
                total += 255 - img.get_pixel(x, y).0[0] as u64;
                n += 1;
            }
        }
        total as f64 / n as f64
    }

    fn numbers(doc: &Document, dict: &lopdf::Dictionary, key: &[u8]) -> [f32; 4] {
        let n: Vec<f32> = dict
            .get_deref(key, doc)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap())
            .collect();
        [n[0].min(n[2]), n[1].min(n[3]), n[0].max(n[2]), n[1].max(n[3])]
    }

    #[test]
    fn fill_renders_text_and_irs_checkbox_states() {
        let blank = normalize(F1040).unwrap();
        let filled = fill(
            &blank,
            &values(&[
                (FIRST_NAME, Value::Text("Ada M".into())),
                (SSN, Value::Text("123456789".into())),
                (SINGLE, Value::Bool(true)),
            ]),
        )
        .unwrap();

        let fields = read_fields(&filled).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(FIRST_NAME).as_deref(), Some("Ada M"));
        assert_eq!(value(SINGLE).as_deref(), Some("1"));
        assert_eq!(value(MFJ), None);

        for field in [FIRST_NAME, SSN, SINGLE] {
            let (before, after) = (ink(&blank, field), ink(&filled, field));
            assert!(
                after > before + 3.0,
                "{field} not visibly filled: ink {before:.1} -> {after:.1}"
            );
        }
        assert!(
            (ink(&filled, MFJ) - ink(&blank, MFJ)).abs() < 1.0,
            "neighbouring box changed"
        );
    }

    #[test]
    fn refill_keeps_earlier_values_and_can_uncheck() {
        let blank = normalize(F1040).unwrap();
        let first = fill(
            &blank,
            &values(&[(FIRST_NAME, Value::Text("Ada".into())), (SINGLE, Value::Bool(true))]),
        )
        .unwrap();
        let second = fill(
            &first,
            &values(&[(SINGLE, Value::Bool(false)), (MFJ, Value::Text("2".into()))]),
        )
        .unwrap();
        let fields = read_fields(&second).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(FIRST_NAME).as_deref(), Some("Ada"));
        assert_eq!(value(SINGLE), None);
        assert_eq!(value(MFJ).as_deref(), Some("2"));
        assert!(ink(&second, MFJ) > ink(&blank, MFJ) + 3.0);
    }

    #[test]
    fn flatten_keeps_the_ink_and_drops_the_form() {
        let filled = fill(
            &normalize(F1040).unwrap(),
            &values(&[(FIRST_NAME, Value::Text("Ada".into())), (SINGLE, Value::Bool(true))]),
        )
        .unwrap();
        let flat = flatten(pdfium(), &filled).unwrap();
        assert!(read_fields(&flat).unwrap().is_empty());
        let text = |pdf: &[u8]| {
            let doc = pdfium().load_pdf_from_byte_slice(pdf, None).unwrap();
            doc.pages().first().unwrap().text().unwrap().all()
        };
        assert!(
            text(&flat).contains("Ada"),
            "flattened page text lacks the filled value"
        );
        assert!(
            !text(&filled).contains("Ada"),
            "value should only become page text once flattened"
        );
    }

    #[test]
    fn page_text_comes_from_the_text_layer() {
        let filled = fill(
            &normalize(F1040).unwrap(),
            &values(&[(FIRST_NAME, Value::Text("Ada".into()))]),
        )
        .unwrap();
        let pages = crate::text::page_texts(pdfium(), &flatten(pdfium(), &filled).unwrap()).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].contains("U.S. Individual Income Tax Return") && pages[0].contains("Ada"));
    }

    #[test]
    fn unique_suffixes_resolve_to_full_names() {
        let blank = normalize(F1040).unwrap();
        let filled = fill(
            &blank,
            &values(&[
                ("Page1[0].f1_14[0]", Value::Text("Ada".into())),
                ("f1_15[0]", Value::Text("Lovelace".into())),
            ]),
        )
        .unwrap();
        let fields = read_fields(&filled).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(FIRST_NAME).as_deref(), Some("Ada"));
        assert_eq!(
            value("topmostSubform[0].Page1[0].f1_15[0]").as_deref(),
            Some("Lovelace")
        );

        let err = fill(
            &blank,
            &values(&[
                ("c1_8[0]", Value::Bool(true)),
                ("f1_14[0]", Value::Text("A".into())),
                (FIRST_NAME, Value::Text("B".into())),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("c1_8[0]: ambiguous; matches topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0], topmostSubform[0].Page1[0].c1_8[0]"), "{err}");
        assert!(err.contains("same field as"), "{err}");

        let err = fill(
            &blank,
            &values(&[
                ("Page1[0].f1_20[0]", Value::Text("1 Main".into())),
                ("Page1[0].zz_99[0]", Value::Bool(true)),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("Page1[0].f1_20[0]: no such field; did you mean topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_20[0]?"),
            "{err}"
        );
        assert!(
            err.contains("zz_99[0]: no such field\n") || err.ends_with("zz_99[0]: no such field"),
            "{err}"
        );
    }

    #[test]
    fn invalid_values_are_all_reported_and_nothing_is_written() {
        let blank = normalize(F1040).unwrap();
        let err = fill(
            &blank,
            &values(&[
                ("nope", Value::Text("x".into())),
                (SSN, Value::Text("123-45-6789".into())),
                (FIRST_NAME, Value::Bool(true)),
                (SINGLE, Value::Text("Yes".into())),
            ]),
        )
        .unwrap_err()
        .to_string();
        for needle in [
            "nope: no such field",
            "exceeds this field's limit of 9; enter it without dashes or spaces",
            "needs a string",
            "not a state of this box",
        ] {
            assert!(err.contains(needle), "missing {needle:?} in:\n{err}");
        }
    }
}
