//! zlib streams for the test files. A build with the archive has zlib's own encoder (the archive re-creates
//! its streams exactly); a build without it writes stored blocks, which every reader decodes and every effort
//! makes smaller, as it does zlib's streams. Tests that need zlib's own bytes (a file of an exact size, a
//! stream the archive rebuilds) run only with the archive.

/// zlib's own stream at `level` (1 to 9).
#[cfg(feature = "archive")]
pub fn zlib(data: &[u8], level: u8) -> Vec<u8> {
    use gpress::flate::zlibexact::{zlib_stream, Params};
    zlib_stream(data, Params { level, window_bits: 15, mem_level: 8, filtered: false })
}

/// Without the archive, and so without its copy of zlib's encoder: the data in stored blocks, at any level.
#[cfg(not(feature = "archive"))]
pub fn zlib(data: &[u8], _level: u8) -> Vec<u8> {
    let mut raw = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 5);
    let mut blocks = data.chunks(65535).peekable();
    if blocks.peek().is_none() {
        raw.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(b) = blocks.next() {
        // The block's header (final or not, stored), then its length and the length's complement.
        let n = b.len() as u16;
        raw.push(blocks.peek().is_none() as u8);
        raw.extend_from_slice(&n.to_le_bytes());
        raw.extend_from_slice(&(!n).to_le_bytes());
        raw.extend_from_slice(b);
    }
    gpress::flate::zlib::wrap(&raw, data)
}
