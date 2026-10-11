//! Form filling via lopdf: text values get generated appearance streams, checkbox/radio
//! states use each widget's own on-state; pdfium flattens.

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object};
use pdfium_render::prelude::*;
use std::collections::{BTreeMap, HashMap};

use crate::acroform::{self, FieldKind, FieldNode};
use crate::appearance::{TextStyle, set_text_appearance};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Bool(bool),
}

/// What a fill wrote, by full field name, and what it rejected.
pub struct Filled {
    /// The new PDF, or None when no value was valid.
    pub pdf: Option<Vec<u8>>,
    pub written: Vec<Written>,
    pub problems: Vec<String>,
    /// Dotted prefix shared by every field in the form.
    pub prefix: String,
}

pub struct Written {
    pub name: String,
    pub description: Option<String>,
    pub value: String,
}

/// Fill `values` into `pdf`. A name may abbreviate a field's full name (see `resolve_name`).
/// Valid values are written even when others are rejected.
pub fn fill(pdf: &[u8], values: &BTreeMap<String, Value>) -> Result<Filled> {
    let mut doc = Document::load_mem(pdf).context("parsing PDF")?;
    let nodes: HashMap<String, FieldNode> = acroform::collect(&doc)?
        .into_iter()
        .map(|n| (n.field.name.clone(), n))
        .collect();
    let prefix = acroform::common_prefix(nodes.keys().map(String::as_str)).to_string();

    let mut texts: Vec<(&FieldNode, &str)> = Vec::new();
    let mut buttons: Vec<(&FieldNode, Vec<u8>)> = Vec::new();
    let mut written: Vec<Written> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut claimed: HashMap<&str, &str> = HashMap::new();
    for (name, value) in values {
        let node = match resolve_name(&nodes, name) {
            Ok(node) => node,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        let resolved = match resolve(&node.field.kind, value) {
            Ok(resolved) => resolved,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        if let Some(other) = claimed.insert(node.field.name.as_str(), name) {
            problems.push(format!("{name}: same field as {other}"));
            continue;
        }
        let shown = match resolved {
            Resolved::Text => {
                let Value::Text(s) = value else {
                    unreachable!("resolve checked the variant")
                };
                texts.push((node, s));
                format!("{s:?}")
            }
            Resolved::Button(state) => {
                let shown = if state == b"Off" { "unchecked" } else { "checked" };
                buttons.push((node, state));
                shown.to_string()
            }
        };
        written.push(Written {
            name: node.field.name.clone(),
            description: node.field.description.clone(),
            value: shown,
        });
    }
    if written.is_empty() {
        return Ok(Filled {
            pdf: None,
            written,
            problems,
            prefix,
        });
    }

    let default_da = acroform::default_da(&doc);
    for (node, text) in &texts {
        doc.get_object_mut(node.id)?
            .as_dict_mut()?
            .set("V", lopdf::text_string(text));
        for widget in &node.widgets {
            let own_da = doc
                .get_dictionary(widget.id)?
                .get(b"DA")
                .ok()
                .and_then(|d| lopdf::decode_text_string(d).ok());
            let style = TextStyle {
                da: own_da
                    .or(node.da.clone())
                    .or(default_da.clone())
                    .context("text field has no /DA")?,
                quadding: doc
                    .get_dictionary(widget.id)?
                    .get(b"Q")
                    .and_then(Object::as_i64)
                    .unwrap_or(node.quadding),
                flags: node.flags,
                max_len: node.max_len,
            };
            set_text_appearance(&mut doc, widget.id, &style, text)
                .with_context(|| format!("drawing {}", node.field.name))?;
        }
    }
    for (node, state) in &buttons {
        acroform::set_button_state(&mut doc, node, state)?;
    }
    acroform::strip_xfa(&mut doc)?;
    Ok(Filled {
        pdf: Some(acroform::save(&mut doc)?),
        written,
        problems,
        prefix,
    })
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

/// The field `name` identifies: its exact full name, else the one field whose name ends with it,
/// else the one whose dotted segments contain its segments in order and end with its last.
/// Surrounding quotes are ignored, and a segment without an index means `[0]`, as in XFA SOM.
fn resolve_name<'a>(nodes: &'a HashMap<String, FieldNode>, name: &str) -> Result<&'a FieldNode> {
    let bare = name.trim().trim_matches(['"', '\'']).trim();
    if let Some(node) = nodes.get(bare) {
        return Ok(node);
    }
    let wanted: Vec<String> = bare.split('.').map(indexed).collect();
    let suffix = format!(".{}", wanted.join("."));
    let tiers: [&dyn Fn(&str) -> bool; 2] = [&|full: &str| full.ends_with(&suffix), &|full: &str| {
        abbreviates(&wanted, full)
    }];
    for matches_name in tiers {
        let mut matches: Vec<&FieldNode> = nodes.values().filter(|n| matches_name(&n.field.name)).collect();
        match matches.len() {
            0 => continue,
            1 => return Ok(matches.remove(0)),
            _ => {
                let mut names: Vec<&str> = matches.iter().map(|n| n.field.name.as_str()).collect();
                names.sort_unstable();
                bail!("ambiguous; matches {}", names.join(", "))
            }
        }
    }
    let leaf = wanted.last().map(String::as_str).unwrap_or_default();
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

fn indexed(segment: &str) -> String {
    if segment.ends_with(']') {
        segment.to_string()
    } else {
        format!("{segment}[0]")
    }
}

/// Whether `wanted` segments occur in order in `full`'s, the last matching `full`'s last.
fn abbreviates(wanted: &[String], full: &str) -> bool {
    let segments: Vec<String> = full.split('.').map(indexed).collect();
    let (Some(leaf), Some(full_leaf)) = (wanted.last(), segments.last()) else {
        return false;
    };
    if leaf != full_leaf {
        return false;
    }
    let mut rest = segments[..segments.len() - 1].iter();
    wanted[..wanted.len() - 1].iter().all(|w| rest.any(|s| s == w))
}

enum Resolved {
    Text,
    Button(Vec<u8>),
}

fn resolve(kind: &FieldKind, value: &Value) -> Result<Resolved> {
    match (kind, value) {
        (FieldKind::Text { max_len, multiline }, Value::Text(s)) => {
            if !multiline && s.contains('\n') {
                bail!("contains a line break but this field is a single line");
            }
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
                Value::Text(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => {
                    return resolve(kind, &Value::Bool(s.eq_ignore_ascii_case("true")));
                }
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

    /// The filled PDF, asserting every value was accepted.
    fn written(pdf: &[u8], values: &BTreeMap<String, Value>) -> Vec<u8> {
        let filled = fill(pdf, values).unwrap();
        assert!(filled.problems.is_empty(), "{:?}", filled.problems);
        filled.pdf.unwrap()
    }

    fn problems(pdf: &[u8], pairs: &[(&str, Value)]) -> String {
        fill(pdf, &values(pairs)).unwrap().problems.join("\n")
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
        let filled = written(
            &blank,
            &values(&[
                (FIRST_NAME, Value::Text("Ada M".into())),
                (SSN, Value::Text("123456789".into())),
                (SINGLE, Value::Bool(true)),
            ]),
        );

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
        let first = written(
            &blank,
            &values(&[(FIRST_NAME, Value::Text("Ada".into())), (SINGLE, Value::Bool(true))]),
        );
        let second = written(
            &first,
            &values(&[(SINGLE, Value::Bool(false)), (MFJ, Value::Text("2".into()))]),
        );
        let fields = read_fields(&second).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(FIRST_NAME).as_deref(), Some("Ada"));
        assert_eq!(value(SINGLE), None);
        assert_eq!(value(MFJ).as_deref(), Some("2"));
        assert!(ink(&second, MFJ) > ink(&blank, MFJ) + 3.0);
    }

    #[test]
    fn flatten_keeps_the_ink_and_drops_the_form() {
        let filled = written(
            &normalize(F1040).unwrap(),
            &values(&[(FIRST_NAME, Value::Text("Ada".into())), (SINGLE, Value::Bool(true))]),
        );
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
        let filled = written(
            &normalize(F1040).unwrap(),
            &values(&[(FIRST_NAME, Value::Text("Ada".into()))]),
        );
        let pages = crate::text::page_texts(pdfium(), &flatten(pdfium(), &filled).unwrap()).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].contains("U.S. Individual Income Tax Return") && pages[0].contains("Ada"));
    }

    #[test]
    fn abbreviated_names_resolve_to_full_names() {
        let blank = normalize(F1040).unwrap();
        let filled = written(
            &blank,
            &values(&[
                ("Page1[0].f1_14[0]", Value::Text("Ada".into())),
                ("f1_15[0]", Value::Text("Lovelace".into())),
                ("Page1.f1_16", Value::Text("123456789".into())),
                ("'Page1[0].f1_20[0]'", Value::Text("1 Main".into())),
                ("Address_ReadOrder.f1_22", Value::Text("Denver".into())),
            ]),
        );
        let fields = read_fields(&filled).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(FIRST_NAME).as_deref(), Some("Ada"));
        assert_eq!(
            value("topmostSubform[0].Page1[0].f1_15[0]").as_deref(),
            Some("Lovelace")
        );
        assert_eq!(value(SSN).as_deref(), Some("123456789"));
        assert_eq!(
            value("topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_20[0]").as_deref(),
            Some("1 Main")
        );
        assert_eq!(
            value("topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_22[0]").as_deref(),
            Some("Denver")
        );
    }

    #[test]
    fn every_listed_name_resolves_to_its_own_field() {
        let doc = Document::load_mem(&normalize(F1040).unwrap()).unwrap();
        let nodes: HashMap<String, FieldNode> = acroform::collect(&doc)
            .unwrap()
            .into_iter()
            .map(|n| (n.field.name.clone(), n))
            .collect();
        let prefix = acroform::common_prefix(nodes.keys().map(String::as_str));
        for full in nodes.keys() {
            let shown = full.strip_prefix(prefix).unwrap();
            assert_eq!(resolve_name(&nodes, shown).unwrap().field.name, *full, "{shown}");
        }
    }

    #[test]
    fn unresolvable_names_are_explained() {
        let blank = normalize(F1040).unwrap();
        let err = problems(
            &blank,
            &[
                ("c1_8[0]", Value::Bool(true)),
                ("f1_14[0]", Value::Text("A".into())),
                (FIRST_NAME, Value::Text("B".into())),
                ("Page2[0].f1_20[0]", Value::Text("1 Main".into())),
                ("Page1[0].zz_99[0]", Value::Bool(true)),
            ],
        );
        assert!(err.contains("c1_8[0]: ambiguous; matches topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0], topmostSubform[0].Page1[0].c1_8[0]"), "{err}");
        assert!(err.contains("same field as"), "{err}");
        assert!(
            err.contains("Page2[0].f1_20[0]: no such field; did you mean topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_20[0]?"),
            "{err}"
        );
        assert!(err.contains("Page1[0].zz_99[0]: no such field"), "{err}");
    }

    #[test]
    fn valid_values_are_written_and_every_invalid_one_is_reported() {
        let blank = normalize(F1040).unwrap();
        let filled = fill(
            &blank,
            &values(&[
                ("nope", Value::Text("x".into())),
                (SSN, Value::Text("123-45-6789".into())),
                (FIRST_NAME, Value::Bool(true)),
                (SINGLE, Value::Text("Yes".into())),
                ("f1_15[0]", Value::Text("Lovelace".into())),
                (MFJ, Value::Text("TRUE".into())),
            ]),
        )
        .unwrap();
        let err = filled.problems.join("\n");
        for needle in [
            "nope: no such field",
            "exceeds this field's limit of 9; enter it without dashes or spaces",
            "needs a string",
            "not a state of this box",
        ] {
            assert!(err.contains(needle), "missing {needle:?} in:\n{err}");
        }
        assert_eq!(filled.problems.len(), 4);
        assert_eq!(filled.prefix, "topmostSubform[0].");
        let shown: Vec<(&str, &str)> = filled
            .written
            .iter()
            .map(|w| (w.name.as_str(), w.value.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("topmostSubform[0].Page1[0].f1_15[0]", "\"Lovelace\""),
                (MFJ, "checked")
            ]
        );
        let fields = read_fields(&filled.pdf.unwrap()).unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value(MFJ).as_deref(), Some("2"));
        assert_eq!(value(SSN), None);
    }

    #[test]
    fn an_invalid_alias_does_not_block_the_valid_full_name() {
        let filled = fill(
            &normalize(F1040).unwrap(),
            &values(&[("f1_14[0]", Value::Bool(true)), (FIRST_NAME, Value::Text("Ada".into()))]),
        )
        .unwrap();
        assert_eq!(filled.problems, ["f1_14[0]: text field needs a string value"]);
        assert_eq!(filled.written.len(), 1);
        let err = problems(
            &normalize(F1040).unwrap(),
            &[(FIRST_NAME, Value::Text("Ada\nLovelace".into()))],
        );
        assert!(err.contains("line break"), "{err}");
    }

    #[test]
    fn nothing_valid_writes_nothing() {
        let filled = fill(&normalize(F1040).unwrap(), &values(&[("nope", Value::Bool(true))])).unwrap();
        assert!(filled.pdf.is_none() && filled.written.is_empty());
        assert_eq!(filled.problems, ["nope: no such field"]);
    }
}
