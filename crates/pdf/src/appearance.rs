//! Text-field appearance streams (`/AP /N`), so filled values render in every
//! viewer and survive flattening and printing.

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

const FF_MULTILINE: i64 = 1 << 12;
const FF_COMB: i64 = 1 << 24;
const PADDING: f32 = 2.0;
const AUTO_SIZE_MAX: f32 = 12.0;
const AUTO_SIZE_MIN: f32 = 4.0;
const LINE_SPACING: f32 = 1.15;
/// Helvetica widths for ASCII 32..=126, used when a font has no `/Widths`.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556,
    556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833,
    722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556,
    556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334,
    260, 334, 584,
];

/// What a text field looks like, resolved from its `/DA`, `/Q`, `/Ff`, and `/MaxLen`.
pub(crate) struct TextStyle {
    pub da: String,
    pub quadding: i64,
    pub flags: i64,
    pub max_len: Option<i64>,
}

/// Build and attach a normal appearance for `value` on one text widget.
pub(crate) fn set_text_appearance(doc: &mut Document, widget: ObjectId, style: &TextStyle, value: &str) -> Result<()> {
    let (font_name, size, color) = parse_da(&style.da)?;
    let font_ref = form_font(doc, &font_name)?;
    let metrics = FontMetrics::read(doc, &font_ref)?;

    let rect = rect(doc.get_dictionary(widget)?, doc)?;
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let bytes = win_ansi(value);
    let multiline = style.flags & FF_MULTILINE != 0;
    let comb = style.flags & FF_COMB != 0 && !multiline;

    let size = match size {
        s if s > 0.0 => s,
        _ if multiline => AUTO_SIZE_MAX.min(h - 2.0 * PADDING).max(AUTO_SIZE_MIN),
        _ => {
            let by_height = (h - 2.0 * PADDING) / (metrics.ascent - metrics.descent);
            let units = metrics.width(&bytes).max(1.0);
            let by_width = (w - 2.0 * PADDING) * 1000.0 / units;
            by_height.min(by_width).clamp(AUTO_SIZE_MIN, AUTO_SIZE_MAX)
        }
    };
    let text_w = |b: &[u8]| metrics.width(b) * size / 1000.0;
    let x_for = |b: &[u8]| match style.quadding {
        1 => (w - text_w(b)) / 2.0,
        2 => w - PADDING - text_w(b),
        _ => PADDING,
    };

    let mut ops = format!(
        "/Tx BMC q {PADDING} {PADDING} {} {} re W n BT /{font_name} {size} Tf {color}\n",
        w - 2.0 * PADDING,
        h - 2.0 * PADDING
    );
    if comb {
        let cells = style.max_len.filter(|&n| n > 0).context("comb field without /MaxLen")? as f32;
        let cell = w / cells;
        let baseline = centered_baseline(h, size, &metrics);
        for (i, b) in bytes.iter().enumerate() {
            let x = i as f32 * cell + (cell - text_w(&[*b])) / 2.0;
            ops.push_str(&format!("1 0 0 1 {x:.2} {baseline:.2} Tm {} Tj\n", literal(&[*b])));
        }
    } else if multiline {
        let line_h = size * LINE_SPACING;
        let mut y = h - PADDING - metrics.ascent * size;
        for line in wrap(&bytes, w - 2.0 * PADDING, &text_w) {
            ops.push_str(&format!(
                "1 0 0 1 {:.2} {y:.2} Tm {} Tj\n",
                x_for(&line),
                literal(&line)
            ));
            y -= line_h;
        }
    } else {
        let baseline = centered_baseline(h, size, &metrics);
        ops.push_str(&format!(
            "1 0 0 1 {:.2} {baseline:.2} Tm {} Tj\n",
            x_for(&bytes),
            literal(&bytes)
        ));
    }
    ops.push_str("ET Q EMC");

    let mut resources = Dictionary::new();
    resources.set(
        "Font",
        Dictionary::from_iter([(font_name.as_bytes().to_vec(), font_ref)]),
    );
    let mut form = Dictionary::new();
    form.set("Type", Object::Name(b"XObject".to_vec()));
    form.set("Subtype", Object::Name(b"Form".to_vec()));
    form.set("BBox", Object::Array(vec![0.into(), 0.into(), w.into(), h.into()]));
    form.set("Resources", resources);
    let stream_id = doc.add_object(Stream::new(form, ops.into_bytes()));

    doc.get_object_mut(widget)?
        .as_dict_mut()?
        .set("AP", Dictionary::from_iter([("N", Object::Reference(stream_id))]));
    Ok(())
}

/// The form's `/DR` font resource named `font_name`.
fn form_font(doc: &Document, font_name: &str) -> Result<Object> {
    doc.catalog()?
        .get_deref(b"AcroForm", doc)?
        .as_dict()?
        .get_deref(b"DR", doc)
        .and_then(|dr| dr.as_dict()?.get_deref(b"Font", doc))
        .and_then(|fonts| fonts.as_dict()?.get(font_name.as_bytes()).cloned())
        .with_context(|| format!("font /{font_name} not in the form's /DR"))
}

struct FontMetrics {
    first_char: i64,
    widths: Vec<f32>,
    missing: f32,
    ascent: f32,
    descent: f32,
}

impl FontMetrics {
    fn read(doc: &Document, font: &Object) -> Result<Self> {
        let font = doc.dereference(font)?.1.as_dict()?;
        let descriptor = font
            .get_deref(b"FontDescriptor", doc)
            .ok()
            .and_then(|d| d.as_dict().ok());
        let number = |d: Option<&Dictionary>, key: &[u8]| d?.get_deref(key, doc).ok()?.as_float().ok();
        let widths: Vec<f32> = font
            .get_deref(b"Widths", doc)
            .and_then(Object::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|w| doc.dereference(w).ok()?.1.as_float().ok())
                    .collect()
            })
            .unwrap_or_default();
        let (first_char, widths) = if widths.is_empty() {
            (32, HELVETICA_WIDTHS.iter().map(|&w| f32::from(w)).collect())
        } else {
            (
                font.get_deref(b"FirstChar", doc).and_then(Object::as_i64).unwrap_or(0),
                widths,
            )
        };
        Ok(Self {
            first_char,
            widths,
            missing: number(descriptor, b"MissingWidth")
                .filter(|&m| m > 0.0)
                .unwrap_or(556.0),
            ascent: number(descriptor, b"Ascent").filter(|&a| a > 0.0).unwrap_or(718.0) / 1000.0,
            descent: number(descriptor, b"Descent").filter(|&d| d < 0.0).unwrap_or(-207.0) / 1000.0,
        })
    }

    /// Width of `text` in thousandths of the font size.
    fn width(&self, text: &[u8]) -> f32 {
        text.iter()
            .map(|&b| {
                usize::try_from(i64::from(b) - self.first_char)
                    .ok()
                    .and_then(|i| self.widths.get(i).copied())
                    .filter(|&w| w > 0.0)
                    .unwrap_or(self.missing)
            })
            .sum()
    }
}

fn centered_baseline(h: f32, size: f32, m: &FontMetrics) -> f32 {
    (h - (m.ascent - m.descent) * size) / 2.0 - m.descent * size
}

fn wrap(text: &[u8], max_w: f32, text_w: &dyn Fn(&[u8]) -> f32) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    for paragraph in text.split(|&b| b == b'\n') {
        let mut line: Vec<u8> = Vec::new();
        for word in paragraph.split(|&b| b == b' ').filter(|w| !w.is_empty()) {
            let candidate = if line.is_empty() {
                word.to_vec()
            } else {
                [&line[..], b" ", word].concat()
            };
            if text_w(&candidate) <= max_w || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_vec()));
            }
        }
        lines.push(line);
    }
    lines
}

/// `/Font size Tf` plus the color operator, from a `/DA` string.
fn parse_da(da: &str) -> Result<(String, f32, String)> {
    let tokens: Vec<&str> = da.split_whitespace().collect();
    let tf = tokens.iter().position(|t| *t == "Tf").context("/DA has no Tf")?;
    if tf < 2 {
        bail!("malformed /DA {da:?}");
    }
    let font = tokens[tf - 2].trim_start_matches('/').to_string();
    let size: f32 = tokens[tf - 1]
        .parse()
        .with_context(|| format!("font size in /DA {da:?}"))?;
    let color = tokens
        .iter()
        .rposition(|t| matches!(*t, "g" | "rg" | "k"))
        .map(|end| {
            let arity = match tokens[end] {
                "g" => 1,
                "rg" => 3,
                _ => 4,
            };
            tokens[end.saturating_sub(arity)..=end].join(" ")
        })
        .unwrap_or_else(|| "0 g".to_string());
    Ok((font, size, color))
}

fn rect(widget: &Dictionary, doc: &Document) -> Result<[f32; 4]> {
    let n: Vec<f32> = widget
        .get_deref(b"Rect", doc)?
        .as_array()?
        .iter()
        .map(|o| o.as_float())
        .collect::<lopdf::Result<_>>()?;
    let [x1, y1, x2, y2] = n[..] else {
        bail!("widget /Rect is not 4 numbers")
    };
    Ok([x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)])
}

/// Encode as WinAnsi (the encoding of every IRS form font); unmappable chars become `?`.
fn win_ansi(text: &str) -> Vec<u8> {
    const SPECIALS: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('‘', 0x91),
        ('’', 0x92),
        ('“', 0x93),
        ('”', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    text.chars()
        .filter(|c| *c != '\r')
        .map(|c| match c as u32 {
            0x0A | 0x20..=0x7E | 0xA0..=0xFF => c as u8,
            _ => SPECIALS.iter().find(|(s, _)| *s == c).map_or(b'?', |(_, b)| *b),
        })
        .collect()
}

fn literal(bytes: &[u8]) -> String {
    let mut out = String::from("(");
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da_parsing() {
        let (font, size, color) = parse_da("/HelveticaLTStd-Bold 8.00 Tf 0.000 0.000 0.502 rg").unwrap();
        assert_eq!(
            (font.as_str(), size, color.as_str()),
            ("HelveticaLTStd-Bold", 8.0, "0.000 0.000 0.502 rg")
        );
        assert_eq!(parse_da("/Helv 0 Tf 0 g").unwrap(), ("Helv".into(), 0.0, "0 g".into()));
        assert!(parse_da("0 g").is_err());
    }

    #[test]
    fn encoding_and_escaping() {
        assert_eq!(win_ansi("Café — “ok” 漢"), b"Caf\xe9 \x97 \x93ok\x94 ?");
        assert_eq!(literal(b"a(b)\\c\xe9"), "(a\\(b\\)\\\\c\\351)");
    }

    #[test]
    fn wrapping_respects_width_and_newlines() {
        let w = |b: &[u8]| b.len() as f32;
        assert_eq!(
            wrap(b"aa bb cc\ndd", 5.0, &w),
            vec![b"aa bb".to_vec(), b"cc".to_vec(), b"dd".to_vec()]
        );
        assert_eq!(
            wrap(b"toolongword x", 3.0, &w),
            vec![b"toolongword".to_vec(), b"x".to_vec()]
        );
    }
}
