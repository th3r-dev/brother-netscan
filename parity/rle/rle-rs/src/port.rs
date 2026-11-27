//! The Rust port of `rle.py`.
//!
//! The decoder lives in the `brother-netscan` crate (`PackBits`, a streaming version that
//! accepts runs split across network records); this port feeds it the whole input at once.

#![forbid(unsafe_code)]

/// Port of `rle_decode` (`rle.py:4`).
pub fn rle_decode(data: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len() * 2);
    brother_netscan::PackBits::default().feed(&data, &mut out);
    Ok(out)
}
