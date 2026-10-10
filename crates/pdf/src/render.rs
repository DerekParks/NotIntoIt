//! Page rasterization (form values included) to PNG.

use anyhow::{Result, bail};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use pdfium_render::prelude::*;

pub struct RenderedPage {
    pub png: Vec<u8>,
    pub page_count: u32,
}

/// Render 1-based `page` of `pdf` at `width` pixels.
pub fn render_page(pdfium: &Pdfium, pdf: &[u8], page: u32, width: u32) -> Result<RenderedPage> {
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let page_count = u32::try_from(document.pages().len())?;
    if page == 0 || page > page_count {
        bail!("page {page} out of range (document has {page_count})");
    }
    let image = document
        .pages()
        .get((page - 1) as PdfPageIndex)?
        .render_with_config(
            &PdfRenderConfig::new()
                .set_target_width(width as i32)
                .render_form_data(true),
        )?
        .as_image()?;
    let mut png = Vec::new();
    let encoder = PngEncoder::new_with_quality(&mut png, CompressionType::Best, FilterType::Adaptive);
    image.to_rgb8().write_with_encoder(encoder)?;
    Ok(RenderedPage { png, page_count })
}
