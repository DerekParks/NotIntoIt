//! Shared helpers for M0 spike binaries. Throwaway quality: crude on purpose.

/// Strip XML tags and collapse whitespace. Good enough for latency spikes;
/// real USLM parsing (section-aware chunking) is M1 work.
pub fn strip_tags(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 2);
    let mut in_tag = false;
    let mut last_ws = true;
    for c in xml.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_ws {
                    out.push(' ');
                    last_ws = true;
                }
            }
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_ws {
                    out.push(' ');
                    last_ws = true;
                }
            }
            c => {
                out.push(c);
                last_ws = false;
            }
        }
    }
    out
}

/// Split text into ~target_chars chunks on word boundaries.
pub fn chunk_text(text: &str, target_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::with_capacity(target_chars + 64);
    for word in text.split(' ') {
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if current.len() >= target_chars {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if current.len() > 100 {
        chunks.push(current);
    }
    chunks
}

/// Quantize a unit-normalized f32 embedding to int8.
pub fn quantize_i8(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|&x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

/// Reinterpret an i8 slice as bytes for SQLite blob binding.
pub fn i8_bytes(v: &[i8]) -> &[u8] {
    // SAFETY: i8 and u8 have identical size and alignment.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len()) }
}

#[derive(Default)]
pub struct Timings(Vec<f64>);

impl Timings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, elapsed: std::time::Duration) {
        self.0.push(elapsed.as_secs_f64() * 1000.0);
    }

    pub fn report(&mut self, label: &str) {
        assert!(!self.0.is_empty(), "no samples for {label}");
        self.0.sort_by(|a, b| a.total_cmp(b));
        let p = |q: f64| self.0[((self.0.len() - 1) as f64 * q) as usize];
        println!(
            "{label:<44} n={:<3} p50={:>8.2}ms p95={:>8.2}ms max={:>8.2}ms",
            self.0.len(),
            p(0.5),
            p(0.95),
            p(1.0)
        );
    }
}
