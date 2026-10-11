//! AcroForm field tree via lopdf: enumeration, XFA normalization, and button-state writes
//! using each widget's real on-state from `AP /N` (IRS boxes use `/1`, `/2`, ...).

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;
use std::collections::HashMap;

use crate::xfa;

const FF_RADIO: i64 = 1 << 15;
const FF_PUSHBUTTON: i64 = 1 << 16;
const FF_MULTILINE: i64 = 1 << 12;

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub name: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 1-based page of the field's first widget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    Text {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_len: Option<u32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        multiline: bool,
    },
    /// Checkbox; `on_states` lists every widget's on-state (one for a plain box).
    Checkbox {
        on_states: Vec<String>,
    },
    Radio {
        states: Vec<String>,
    },
    Other {
        pdf_type: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Widget {
    pub id: ObjectId,
    pub page: Option<u32>,
    pub on_state: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub(crate) struct FieldNode {
    pub field: Field,
    pub id: ObjectId,
    pub widgets: Vec<Widget>,
    pub da: Option<String>,
    pub quadding: i64,
    pub flags: i64,
    pub max_len: Option<i64>,
}

/// Every terminal field in the document, in AcroForm order.
pub fn read_fields(pdf: &[u8]) -> Result<Vec<Field>> {
    let doc = Document::load_mem(pdf).context("parsing PDF")?;
    Ok(collect(&doc)?.into_iter().map(|n| n.field).collect())
}

/// Copy XFA field descriptions into AcroForm `/TU` tooltips and drop the XFA layer.
pub fn normalize(pdf: &[u8]) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(pdf).context("parsing PDF")?;
    let descriptions = match xfa_template(&doc)? {
        Some(template) => xfa::descriptions(&template)?,
        None => HashMap::new(),
    };
    for node in collect(&doc)? {
        let dict = doc.get_object_mut(node.id)?.as_dict_mut()?;
        if !dict.has(b"TU")
            && let Some(d) = descriptions.get(&node.field.name)
        {
            dict.set("TU", lopdf::text_string(d));
        }
    }
    strip_xfa(&mut doc)?;
    save(&mut doc)
}

pub(crate) fn strip_xfa(doc: &mut Document) -> Result<()> {
    if let Some(acroform) = acroform_mut(doc)? {
        acroform.remove(b"XFA");
    }
    doc.catalog_mut()?.remove(b"NeedsRendering");
    Ok(())
}

pub(crate) fn save(doc: &mut Document) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

/// The AcroForm-level default `/DA`.
pub(crate) fn default_da(doc: &Document) -> Option<String> {
    acroform(doc)
        .ok()??
        .get_deref(b"DA", doc)
        .ok()
        .and_then(|da| lopdf::decode_text_string(da).ok())
}

/// Set a checkbox/radio field to `state` (an on-state name, or `Off`): `/V` on
/// the field, `/AS` on each widget — on if that widget owns `state`, else Off.
pub(crate) fn set_button_state(doc: &mut Document, node: &FieldNode, state: &[u8]) -> Result<()> {
    doc.get_object_mut(node.id)?
        .as_dict_mut()?
        .set("V", Object::Name(state.to_vec()));
    for widget in &node.widgets {
        let appearance = if widget.on_state.as_deref() == Some(state) {
            state
        } else {
            b"Off"
        };
        doc.get_object_mut(widget.id)?
            .as_dict_mut()?
            .set("AS", Object::Name(appearance.to_vec()));
    }
    Ok(())
}

pub(crate) fn collect(doc: &Document) -> Result<Vec<FieldNode>> {
    let Some(acroform) = acroform(doc)? else {
        return Ok(Vec::new());
    };
    let pages = widget_pages(doc);
    let mut out = Vec::new();
    if let Ok(fields) = acroform
        .get(b"Fields")
        .and_then(|f| doc.dereference(f))
        .and_then(|(_, f)| f.as_array())
    {
        for field in fields {
            walk(doc, field, &Inherited::default(), &pages, &mut out)?;
        }
    }
    Ok(out)
}

#[derive(Default, Clone)]
struct Inherited {
    name: String,
    ft: Option<Vec<u8>>,
    ff: Option<i64>,
    max_len: Option<i64>,
    value: Option<Object>,
    da: Option<String>,
    quadding: Option<i64>,
}

impl Inherited {
    /// These attributes as seen by a child `dict`, which may set or override each one.
    fn inherited_by(&self, doc: &Document, dict: &Dictionary) -> Result<Inherited> {
        let mut here = self.clone();
        if let Ok(t) = dict.get(b"T") {
            let partial = lopdf::decode_text_string(t)?;
            here.name = if self.name.is_empty() {
                partial
            } else {
                format!("{}.{partial}", self.name)
            };
        }
        if let Ok(ft) = dict.get(b"FT").and_then(Object::as_name) {
            here.ft = Some(ft.to_vec());
        }
        if let Ok(ff) = dict.get_deref(b"Ff", doc).and_then(Object::as_i64) {
            here.ff = Some(ff);
        }
        if let Ok(max_len) = dict.get_deref(b"MaxLen", doc).and_then(Object::as_i64) {
            here.max_len = Some(max_len);
        }
        if let Ok(v) = dict.get(b"V") {
            here.value = Some(v.clone());
        }
        if let Ok(da) = dict.get_deref(b"DA", doc).and_then(lopdf::decode_text_string) {
            here.da = Some(da);
        }
        if let Ok(q) = dict.get_deref(b"Q", doc).and_then(Object::as_i64) {
            here.quadding = Some(q);
        }
        Ok(here)
    }
}

fn walk(
    doc: &Document,
    obj: &Object,
    parent: &Inherited,
    pages: &HashMap<ObjectId, u32>,
    out: &mut Vec<FieldNode>,
) -> Result<()> {
    let Object::Reference(id) = obj else {
        bail!("direct-object form fields are not supported");
    };
    let dict = doc.get_dictionary(*id)?;

    let here = parent.inherited_by(doc, dict)?;

    let (child_fields, widget_kids) = split_kids(doc, dict);
    for child in &child_fields {
        walk(doc, child, &here, pages, out)?;
    }
    if !child_fields.is_empty() && widget_kids.is_empty() {
        return Ok(());
    }

    let widgets = widgets(doc, *id, &widget_kids, pages);

    let ff = here.ff.unwrap_or(0);
    let kind = field_kind(&here, &widgets);
    let value = here.value.as_ref().and_then(value_text);
    let description = dict.get(b"TU").ok().and_then(|tu| lopdf::decode_text_string(tu).ok());

    out.push(FieldNode {
        field: Field {
            name: here.name,
            kind,
            description: description.filter(|d| !d.trim().is_empty()),
            page: widgets.iter().find_map(|w| w.page),
            value: value.filter(|v| !v.is_empty() && v != "Off"),
        },
        id: *id,
        widgets,
        da: here.da,
        quadding: here.quadding.unwrap_or(0),
        flags: ff,
        max_len: here.max_len,
    });
    Ok(())
}

/// A field's `/Kids`, split into child fields (which have a `/T`) and widget annotations.
fn split_kids(doc: &Document, dict: &Dictionary) -> (Vec<Object>, Vec<Object>) {
    let kids: Vec<Object> = dict
        .get(b"Kids")
        .and_then(|k| doc.dereference(k))
        .and_then(|(_, k)| k.as_array())
        .cloned()
        .unwrap_or_default();
    kids.into_iter().partition(|k| {
        doc.dereference(k)
            .ok()
            .and_then(|(_, o)| o.as_dict().ok())
            .is_some_and(|d| d.has(b"T"))
    })
}

/// The widgets of field `id`: its widget kids, or the field itself when it has none.
fn widgets(doc: &Document, id: ObjectId, widget_kids: &[Object], pages: &HashMap<ObjectId, u32>) -> Vec<Widget> {
    let widget_ids: Vec<ObjectId> = if widget_kids.is_empty() {
        vec![id]
    } else {
        widget_kids.iter().filter_map(|k| k.as_reference().ok()).collect()
    };
    widget_ids
        .into_iter()
        .map(|wid| {
            let w = doc.get_dictionary(wid).ok();
            Widget {
                id: wid,
                page: pages.get(&wid).copied(),
                on_state: w.and_then(|w| on_state(doc, w)),
            }
        })
        .collect()
}

/// The field's type and states, from its `/FT`, `/Ff` flags, and widget appearances.
fn field_kind(here: &Inherited, widgets: &[Widget]) -> FieldKind {
    let ff = here.ff.unwrap_or(0);
    match here.ft.as_deref() {
        Some(b"Tx") => FieldKind::Text {
            max_len: here.max_len.and_then(|m| u32::try_from(m).ok()),
            multiline: ff & FF_MULTILINE != 0,
        },
        Some(b"Btn") if ff & FF_PUSHBUTTON == 0 => {
            let states = dedup(widgets.iter().filter_map(|w| w.on_state.as_deref()).map(name_str));
            if ff & FF_RADIO != 0 {
                FieldKind::Radio { states }
            } else {
                FieldKind::Checkbox { on_states: states }
            }
        }
        other => FieldKind::Other {
            pdf_type: other.map(name_str).unwrap_or_else(|| "unknown".into()),
        },
    }
}

fn on_state(doc: &Document, widget: &Dictionary) -> Option<Vec<u8>> {
    let ap = widget.get_deref(b"AP", doc).ok()?.as_dict().ok()?;
    let normal = ap.get_deref(b"N", doc).ok()?.as_dict().ok()?;
    normal.iter().map(|(k, _)| k.clone()).find(|k| k != b"Off")
}

fn widget_pages(doc: &Document) -> HashMap<ObjectId, u32> {
    let mut map = HashMap::new();
    for (number, page_id) in doc.get_pages() {
        let annots = doc
            .get_dictionary(page_id)
            .ok()
            .and_then(|p| p.get_deref(b"Annots", doc).ok())
            .and_then(|a| a.as_array().ok());
        for annot in annots.into_iter().flatten() {
            if let Ok(id) = annot.as_reference() {
                map.insert(id, number);
            }
        }
    }
    map
}

fn acroform(doc: &Document) -> Result<Option<&Dictionary>> {
    match doc.catalog()?.get(b"AcroForm") {
        Ok(obj) => Ok(Some(doc.dereference(obj)?.1.as_dict()?)),
        Err(_) => Ok(None),
    }
}

fn acroform_mut(doc: &mut Document) -> Result<Option<&mut Dictionary>> {
    let reference = match doc.catalog()?.get(b"AcroForm") {
        Ok(Object::Reference(id)) => Some(*id),
        Ok(_) => None,
        Err(_) => return Ok(None),
    };
    Ok(Some(match reference {
        Some(id) => doc.get_object_mut(id)?.as_dict_mut()?,
        None => doc.catalog_mut()?.get_mut(b"AcroForm")?.as_dict_mut()?,
    }))
}

/// The XFA template packet (or the whole XDP when XFA is a single stream).
fn xfa_template(doc: &Document) -> Result<Option<Vec<u8>>> {
    let Some(acroform) = acroform(doc)? else {
        return Ok(None);
    };
    let Ok(xfa) = acroform.get(b"XFA") else { return Ok(None) };
    let stream_bytes =
        |obj: &Object| -> Result<Vec<u8>> { Ok(doc.dereference(obj)?.1.as_stream()?.get_plain_content()?) };
    match doc.dereference(xfa)?.1 {
        Object::Array(parts) => {
            for pair in parts.chunks(2) {
                if let [label, stream] = pair
                    && lopdf::decode_text_string(label).ok().as_deref() == Some("template")
                {
                    return Ok(Some(stream_bytes(stream)?));
                }
            }
            Ok(None)
        }
        Object::Stream(_) => Ok(Some(stream_bytes(xfa)?)),
        _ => Ok(None),
    }
}

/// A `/V` as text: a button state's name, or a text string.
fn value_text(v: &Object) -> Option<String> {
    match v {
        Object::Name(n) => Some(name_str(n)),
        v => lopdf::decode_text_string(v).ok(),
    }
}

fn name_str(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

fn dedup(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const F1040: &[u8] = include_bytes!("../tests/fixtures/f1040--2025.pdf");

    #[test]
    fn raw_irs_form_has_no_tooltips_but_has_xfa() {
        let fields = read_fields(F1040).unwrap();
        assert!(fields.len() > 190, "{} fields", fields.len());
        assert!(fields.iter().all(|f| f.description.is_none()));
        assert!(xfa_template(&Document::load_mem(F1040).unwrap()).unwrap().is_some());
    }

    #[test]
    fn normalize_moves_xfa_descriptions_into_tooltips() {
        let normalized = normalize(F1040).unwrap();
        let doc = Document::load_mem(&normalized).unwrap();
        assert!(xfa_template(&doc).unwrap().is_none(), "XFA must be stripped");

        let fields = read_fields(&normalized).unwrap();
        let described = fields.iter().filter(|f| f.description.is_some()).count();
        assert!(
            described * 100 / fields.len() >= 95,
            "{described}/{} described; missing: {:?}",
            fields.len(),
            fields
                .iter()
                .filter(|f| f.description.is_none())
                .map(|f| &f.name)
                .collect::<Vec<_>>()
        );

        let by_name = |n: &str| fields.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("{n}"));
        let single = by_name("topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0]");
        assert_eq!(
            single.kind,
            FieldKind::Checkbox {
                on_states: vec!["1".into()]
            }
        );
        assert!(single.description.as_deref().unwrap().contains("Single"), "{single:?}");
        assert_eq!(single.page, Some(1));
        assert!(
            by_name("topmostSubform[0].Page1[0].f1_01[0]")
                .description
                .as_deref()
                .unwrap()
                .contains("For the year")
        );
    }
}
