//! S3: pdfium-render round-trip on the real IRS f1040:
//! enumerate fields -> fill text fields + a checkbox -> save -> reload ->
//! read values back -> render page 1 to PNG for visual verification.

use anyhow::{Context, Result};
use pdfium_render::prelude::*;

const SRC: &str = "data/f1040.pdf";
const FILLED: &str = "data/f1040_filled.pdf";
const FIXED: &str = "data/f1040_fixed.pdf";
const PNG: &str = "data/f1040_filled_p1.png";
const PNG_FIXED: &str = "data/f1040_fixed_p1.png";
const N_TEXT_FILLS: usize = 10;
const CHECKBOX: &[u8] = b"c1_1[0]";

/// PDF text strings are either UTF-16BE with a BOM or PDFDocEncoding (~Latin-1).
fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn main() -> Result<()> {
    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./data/pdfium/lib/"))
            .context("binding to pdfium library")?,
    );

    let document = pdfium.load_pdf_from_file(SRC, None)?;
    println!(
        "loaded {SRC}: {} pages, form: {:?}",
        document.pages().len(),
        document.form().map(|f| f.form_type()),
    );

    // -- census -----------------------------------------------------------------
    let mut n_text = 0;
    let mut n_checkbox = 0;
    let mut n_other = 0;
    println!("\n== field census (page 1 detail) ==");
    for (page_index, page) in document.pages().iter().enumerate() {
        for annotation in page.annotations().iter() {
            let Some(field) = annotation.as_form_field() else {
                continue;
            };
            let name = field.name().unwrap_or_default();
            match field.field_type() {
                PdfFormFieldType::Text => {
                    n_text += 1;
                    if page_index == 0 && n_text <= 12 {
                        println!("  text     {name}");
                    }
                }
                PdfFormFieldType::Checkbox => {
                    n_checkbox += 1;
                    let export = field
                        .as_checkbox_field()
                        .and_then(|c| c.group_value())
                        .unwrap_or_default();
                    if page_index == 0 && n_checkbox <= 8 {
                        println!("  checkbox {name} (group_value: {export:?})");
                    }
                }
                other => {
                    n_other += 1;
                    if n_other <= 5 {
                        println!("  other    {name} ({other:?})");
                    }
                }
            }
        }
    }
    println!("totals: {n_text} text, {n_checkbox} checkbox, {n_other} other");

    // -- fill ---------------------------------------------------------------------
    println!("\n== filling ==");
    let mut expected: Vec<(String, String)> = Vec::new();
    let mut checked_boxes: Vec<String> = Vec::new();
    let page = document.pages().first()?;
    for mut annotation in page.annotations().iter() {
        let Some(field) = annotation.as_form_field_mut() else {
            continue;
        };
        let name = field.name().unwrap_or_default();
        if let Some(text_field) = field.as_text_field_mut() {
            if expected.len() < N_TEXT_FILLS {
                let value = format!("TEST{:02}", expected.len());
                text_field.set_value(&value)?;
                expected.push((name.clone(), value.clone()));
                println!("  set  {name} = {value}");
            }
        } else if let Some(checkbox) = field.as_checkbox_field_mut() {
            if checked_boxes.is_empty() {
                checkbox.set_checked(true)?;
                println!("  tick {name}");
                checked_boxes.push(name.clone());
            }
        }
    }
    drop(page);
    document.save_to_file(FILLED)?;
    println!("saved {FILLED}");

    // -- read back ------------------------------------------------------------------
    println!("\n== readback from {FILLED} ==");
    let reloaded = pdfium.load_pdf_from_file(FILLED, None)?;
    let mut failures = 0;
    let page = reloaded.pages().first()?;
    for annotation in page.annotations().iter() {
        let Some(field) = annotation.as_form_field() else {
            continue;
        };
        let name = field.name().unwrap_or_default();
        if let Some((_, want)) = expected.iter().find(|(n, _)| *n == name) {
            let got = field.as_text_field().and_then(|t| t.value());
            let ok = got.as_deref() == Some(want.as_str());
            if !ok {
                failures += 1;
            }
            println!(
                "  {} {name}: want {want:?} got {got:?}",
                if ok { "OK " } else { "FAIL" }
            );
        }
        if checked_boxes.contains(&name) {
            let got = field.as_checkbox_field().map(|c| c.is_checked());
            let ok = matches!(got, Some(Ok(true)));
            if !ok {
                failures += 1;
            }
            println!("  {} {name}: want checked got {got:?}", if ok { "OK " } else { "FAIL" });
        }
    }

    // -- render for visual check -------------------------------------------------------
    {
        let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(1700))?;
        bitmap.as_image()?.save_with_format(PNG, image::ImageFormat::Png)?;
    }
    println!("\nrendered page 1 -> {PNG}");
    drop(page);
    drop(reloaded);

    // -- lopdf post-pass: pdfium's set_checked writes /V=/Yes, but IRS checkboxes
    // name their on-state /1 (etc.) in AP /N. Read the true on-state from the
    // appearance dictionary — exactly what the manifest generator will do — and
    // rewrite /V and /AS to match.
    println!("\n== lopdf post-pass: checkbox on-state from AP /N ==");
    let mut doc = lopdf::Document::load(FILLED)?;
    let mut targets: Vec<(lopdf::ObjectId, Vec<u8>)> = Vec::new();
    for (&id, obj) in doc.objects.iter() {
        let Ok(dict) = obj.as_dict() else { continue };
        let is_target = dict
            .get(b"T")
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(|t| decode_pdf_string(t) == String::from_utf8_lossy(CHECKBOX))
            .unwrap_or(false);
        if !is_target {
            continue;
        }
        let resolve = |o: &lopdf::Object| -> Option<lopdf::Dictionary> {
            match o {
                lopdf::Object::Reference(r) => doc.get_object(*r).ok().and_then(|x| x.as_dict().ok()).cloned(),
                o => o.as_dict().ok().cloned(),
            }
        };
        let on_state = dict
            .get(b"AP")
            .ok()
            .and_then(resolve)
            .and_then(|ap| ap.get(b"N").ok().and_then(resolve))
            .and_then(|n| n.iter().map(|(k, _)| k.clone()).find(|k| k != b"Off"));
        let Some(on_state) = on_state else {
            anyhow::bail!("no on-state found in AP /N for {:?}", String::from_utf8_lossy(CHECKBOX));
        };
        targets.push((id, on_state));
    }
    anyhow::ensure!(!targets.is_empty(), "checkbox widget not found via lopdf");
    for (id, on_state) in targets {
        println!(
            "  {}: /V=/Yes -> /{0} and /AS -> /{0}",
            String::from_utf8_lossy(&on_state)
        );
        let dict = doc.get_object_mut(id)?.as_dict_mut()?;
        dict.set("V", lopdf::Object::Name(on_state.clone()));
        dict.set("AS", lopdf::Object::Name(on_state));
    }
    doc.save(FIXED)?;

    // -- verify: authoritative check is /V and /AS in the saved file (pdfium-render's
    // is_checked() compares against the hardcoded "Yes" state, so it misreports
    // IRS-style /1 on-states — print it as informational only).
    let verify = lopdf::Document::load(FIXED)?;
    let mut checkbox_ok = false;
    for (_, obj) in verify.objects.iter() {
        let Ok(dict) = obj.as_dict() else { continue };
        let is_target = dict
            .get(b"T")
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(|t| decode_pdf_string(t) == String::from_utf8_lossy(CHECKBOX))
            .unwrap_or(false);
        if is_target {
            let v = dict.get(b"V").ok().and_then(|o| o.as_name().ok());
            let a = dict.get(b"AS").ok().and_then(|o| o.as_name().ok());
            checkbox_ok = v == Some(b"1") && a == Some(b"1");
            println!("fixed pdf dict: /V={v:?} /AS={a:?} -> on-state correct: {checkbox_ok}");
        }
    }

    let fixed = pdfium.load_pdf_from_file(FIXED, None)?;
    let page = fixed.pages().first()?;
    for annotation in page.annotations().iter() {
        let Some(field) = annotation.as_form_field() else {
            continue;
        };
        if field.name().unwrap_or_default().ends_with("c1_1[0]") {
            let via_pdfium = field.as_checkbox_field().map(|c| c.is_checked());
            println!("(informational) pdfium is_checked on /1 state: {via_pdfium:?}");
        }
    }
    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(1700))?;
    bitmap
        .as_image()?
        .save_with_format(PNG_FIXED, image::ImageFormat::Png)?;
    println!("rendered fixed page 1 -> {PNG_FIXED} (visually verify the check mark)");

    if failures > 0 || !checkbox_ok {
        anyhow::bail!("{failures} readback failures, checkbox_ok={checkbox_ok}");
    }
    println!("S3 PASS: text fill via pdfium + checkbox on-state via lopdf post-pass");
    Ok(())
}
