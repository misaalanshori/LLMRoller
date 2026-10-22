#![forbid(unsafe_code)]
//! Unbuffered SSE forwarder, 64 KiB pre-first-frame holding buffer, and usage-chunk filter.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1053-pre-first-frame-holding-buffer-and-terminal-error-selection
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#124-pre-first-frame-holding-sse-streaming-tee-and-requestlifecycle-guard
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard

use std::io;

use bytes::Bytes;
use http::StatusCode;
use http::header::HeaderMap;
use llmroller_proto::sse_frame::{
    extract_frame_error, find_frame_delimiter, is_comment_frame, is_injected_usage_frame,
};
use thiserror::Error;

/// Maximum bytes held in pre-first-frame buffer before auto-committing per Section 10.5.3.
pub const HOLD_BUFFER_MAX_BYTES: usize = 65_536;

/// Hard ceiling for a single buffered decompression (SEC-8).
pub const MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

/// Forwarding and stream processing errors.
#[derive(Debug, Error)]
pub enum ForwardError {
    #[error("upstream returned error object before first frame: {0}")]
    UpstreamError(String),
    #[error("unsupported content-encoding: {0}")]
    UnsupportedEncoding(String),
    #[error("decompressed {label} output exceeds the {limit}-byte limit")]
    DecompressionLimit { label: &'static str, limit: usize },
    #[error("decompression I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Buffer holding initial status, headers, and initial bytes until the first valid frame is verified.
#[derive(Debug)]
pub struct PreFirstFrameBuffer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub buffer: Vec<u8>,
    pub committed: bool,
}

impl PreFirstFrameBuffer {
    #[must_use]
    pub fn new(status: StatusCode, headers: HeaderMap) -> Self {
        Self {
            status,
            headers,
            buffer: Vec::with_capacity(8192),
            committed: false,
        }
    }

    /// Feed an initial chunk into the pre-first-frame holding buffer.
    ///
    /// If an error object is encountered before commit, returns `Err(ForwardError::UpstreamError)`.
    /// When the first valid non-comment frame is received or the buffer reaches 64 KiB,
    /// marks `committed = true` and flushes all held bytes.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Option<Bytes>, ForwardError> {
        if self.committed {
            return Ok(Some(Bytes::copy_from_slice(chunk)));
        }

        self.buffer.extend_from_slice(chunk);

        // Check for error object before commit
        if let Some(err_msg) = extract_frame_error(&self.buffer) {
            return Err(ForwardError::UpstreamError(err_msg));
        }

        // Commit condition 1: A complete non-comment frame has arrived
        let mut offset = 0;
        while let Some(idx) = find_frame_delimiter(&self.buffer[offset..]) {
            let frame = &self.buffer[offset..offset + idx];
            if !is_comment_frame(frame) {
                self.committed = true;
                let flushed = std::mem::take(&mut self.buffer);
                return Ok(Some(Bytes::from(flushed)));
            }
            offset += idx;
        }

        // Commit condition 2: Buffer reached 64 KiB without an error object
        if self.buffer.len() >= HOLD_BUFFER_MAX_BYTES {
            self.committed = true;
            let flushed = std::mem::take(&mut self.buffer);
            return Ok(Some(Bytes::from(flushed)));
        }

        Ok(None)
    }

    /// Flush any remaining buffered bytes at stream end.
    pub fn flush_remaining(&mut self) -> Option<Bytes> {
        if !self.buffer.is_empty() {
            self.committed = true;
            let flushed = std::mem::take(&mut self.buffer);
            Some(Bytes::from(flushed))
        } else {
            None
        }
    }
}

/// Chunk-by-chunk SSE forwarder with optional single-frame usage chunk suppression per Section 12.4.1.
#[derive(Debug)]
pub struct SseChunkForwarder {
    pub strip_injected_usage_chunk: bool,
    pub partial: Vec<u8>,
}

impl SseChunkForwarder {
    #[must_use]
    pub fn new(strip_injected_usage_chunk: bool) -> Self {
        Self {
            strip_injected_usage_chunk,
            partial: Vec::with_capacity(8192),
        }
    }

    /// Process an incoming chunk, emitting client-facing bytes with zero delay per PERF-2.
    ///
    /// When `strip_injected_usage_chunk` is active, buffers only the incomplete tail of
    /// the current SSE frame in `partial` (max 64 KiB), suppressing complete frames matching
    /// `"choices":[]` and `"usage":{}`.
    pub fn process_chunk(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        if !self.strip_injected_usage_chunk {
            return vec![Bytes::copy_from_slice(chunk)];
        }

        self.partial.extend_from_slice(chunk);
        let mut output = Vec::new();

        while let Some(idx) = find_frame_delimiter(&self.partial) {
            let frame: Vec<u8> = self.partial.drain(..idx).collect();
            if is_injected_usage_frame(&frame) {
                // Suppress this frame from forwarding to the client!
                continue;
            }
            output.push(Bytes::from(frame));
        }

        // Bound partial buffer at 64 KiB to prevent memory exhaustion
        if self.partial.len() >= HOLD_BUFFER_MAX_BYTES {
            let overflow = std::mem::take(&mut self.partial);
            output.push(Bytes::from(overflow));
        }

        output
    }

    /// Flush any remaining partial bytes at stream completion verbatim.
    pub fn finish(&mut self) -> Option<Bytes> {
        if !self.partial.is_empty() {
            let remaining = std::mem::take(&mut self.partial);
            Some(Bytes::from(remaining))
        } else {
            None
        }
    }
}

/// Read at most [`MAX_DECOMPRESSED_BYTES`] from a decoder, rejecting bombs (SEC-8).
fn read_bounded<R: io::Read>(reader: R, label: &'static str) -> Result<Vec<u8>, ForwardError> {
    use std::io::Read as _;

    let mut limited = reader.take((MAX_DECOMPRESSED_BYTES as u64).saturating_add(1));
    let mut out = Vec::new();
    limited.read_to_end(&mut out).map_err(ForwardError::Io)?;
    if out.len() > MAX_DECOMPRESSED_BYTES {
        return Err(ForwardError::DecompressionLimit {
            label,
            limit: MAX_DECOMPRESSED_BYTES,
        });
    }
    Ok(out)
}

/// Synchronous full-body decompression helper for transparent upstream decompression fallback
/// per Section 12.4 and bounded buffering per SEC-8.
pub fn decompress_chunk(encoding: &str, compressed: &[u8]) -> Result<Vec<u8>, ForwardError> {
    let encoding = encoding.trim().to_ascii_lowercase();
    match encoding.as_str() {
        "gzip" => read_bounded(flate2::read::GzDecoder::new(compressed), "gzip"),
        "deflate" => read_bounded(flate2::read::DeflateDecoder::new(compressed), "deflate"),
        "zlib" => read_bounded(flate2::read::ZlibDecoder::new(compressed), "zlib"),
        "br" => read_bounded(brotli::Decompressor::new(compressed, 4096), "brotli"),
        "zstd" => {
            let decoder = zstd::stream::read::Decoder::new(compressed).map_err(ForwardError::Io)?;
            read_bounded(decoder, "zstd")
        }
        "identity" | "" => Ok(compressed.to_vec()),
        other => Err(ForwardError::UnsupportedEncoding(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;

    #[test]
    #[doc = "Spec: SEC-8 - bounded decompression rejects a gzip bomb above the cap"]
    fn decompress_chunk_rejects_gzip_bomb_above_cap() {
        let payload = vec![0u8; MAX_DECOMPRESSED_BYTES + 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&payload).expect("compress");
        let compressed = encoder.finish().expect("finish");

        let error = decompress_chunk("gzip", &compressed).expect_err("bomb must be rejected");
        assert!(
            matches!(
                error,
                ForwardError::DecompressionLimit {
                    label: "gzip",
                    limit: MAX_DECOMPRESSED_BYTES
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    #[doc = "Spec: TRN-3 - supported encodings decode to the original identity body"]
    fn decompress_chunk_roundtrips_gzip_and_identity() {
        let payload = br#"{"usage":{"total_tokens":7}}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(payload).expect("compress");
        let compressed = encoder.finish().expect("finish");

        assert_eq!(
            decompress_chunk(" GZIP ", &compressed).expect("gzip decode"),
            payload
        );
        assert_eq!(
            decompress_chunk("identity", payload).expect("identity decode"),
            payload
        );
    }
}
