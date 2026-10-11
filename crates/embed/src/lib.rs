//! Local embeddings via fastembed (bge-small-en-v1.5 int8 ONNX), downloaded into the cache dir
//! on first use. `--features cuda` adds onnxruntime's CUDA execution provider.

use anyhow::Result;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::path::Path;

pub const DIM: usize = 384;
/// True when this crate was compiled with the `cuda` feature.
pub const CUDA_ENABLED: bool = cfg!(feature = "cuda");
/// bge-* models expect this prefix on queries (and no prefix on documents).
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

pub struct Embedder {
    model: TextEmbedding,
}

/// Which GPU to use when built with the `cuda` feature. Ignored on CPU builds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Device {
    pub cuda_device_id: Option<i32>,
}

impl Embedder {
    pub fn new(cache_dir: &Path, device: Device) -> Result<Self> {
        let options = TextInitOptions::new(EmbeddingModel::BGESmallENV15Q)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_show_download_progress(true);

        // The GPU is used only when a device id is given.
        #[cfg(feature = "cuda")]
        let options = match device.cuda_device_id {
            Some(id) => {
                use ort::execution_providers::CUDAExecutionProvider;
                let ep = CUDAExecutionProvider::default().with_device_id(id);
                options.with_execution_providers(vec![ep.build().error_on_failure()])
            }
            None => options,
        };
        #[cfg(not(feature = "cuda"))]
        let _ = device;

        Ok(Self {
            model: TextEmbedding::try_new(options)?,
        })
    }

    /// Embed document texts (no prefix), batched.
    pub fn embed_docs(&mut self, texts: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>> {
        self.model.embed(texts, Some(batch_size))
    }

    /// Embed a search query (bge query prefix applied).
    pub fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let mut out = self.model.embed(&[format!("{QUERY_PREFIX}{query}")], None)?;
        Ok(out.pop().expect("one embedding for one input"))
    }
}
