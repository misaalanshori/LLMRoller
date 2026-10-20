//! Integration tests for wire transparency, pre-first-frame holding buffer, and usage filtering.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1053-pre-first-frame-holding-buffer-and-terminal-error-selection
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#124-pre-first-frame-holding-sse-streaming-tee-and-requestlifecycle-guard
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard

use http::StatusCode;
use http::header::HeaderMap;
use llmroller_proxy::forward::{
    ForwardError, HOLD_BUFFER_MAX_BYTES, PreFirstFrameBuffer, SseChunkForwarder, decompress_chunk,
};

/// Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2
/// Verify that upstream status and headers are held in a 64 KiB buffer until the first valid
/// non-comment frame is verified free of an error object, then committed.
#[doc = "Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2"]
#[test]
fn test_pre_first_frame_holding_buffer_and_error_detection() {
    let mut buf = PreFirstFrameBuffer::new(StatusCode::OK, HeaderMap::new());

    // 1. Comment frames (: ping) keep holding
    let chunk1 = b": ping\n\n";
    let res1 = buf.push_chunk(chunk1).expect("push comment");
    assert!(res1.is_none());
    assert!(!buf.committed);

    // 2. Another comment keeps holding
    let chunk2 = b": keep-alive\n\n";
    let res2 = buf.push_chunk(chunk2).expect("push comment 2");
    assert!(res2.is_none());
    assert!(!buf.committed);

    // 3. First valid non-comment frame commits and flushes all held bytes
    let valid_frame = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
    let res3 = buf.push_chunk(valid_frame).expect("push valid");
    assert!(res3.is_some());
    assert!(buf.committed);

    let flushed = res3.expect("flushed chunk");
    let flushed_str = std::str::from_utf8(&flushed).expect("utf8");
    assert!(flushed_str.contains(": ping\n\n"));
    assert!(flushed_str.contains(": keep-alive\n\n"));
    assert!(flushed_str.contains("data: {\"choices\""));

    // 4. Subsequent chunks pass through immediately
    let chunk4 = b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n";
    let res4 = buf.push_chunk(chunk4).expect("push chunk 4");
    assert_eq!(res4.expect("pass through chunk").as_ref(), chunk4);

    // 5. Error detection before commit triggers error
    let mut err_buf = PreFirstFrameBuffer::new(StatusCode::OK, HeaderMap::new());
    let err_chunk =
        b"data: {\"error\":{\"message\":\"upstream failure\",\"type\":\"server_error\"}}\n\n";
    let err_res = err_buf
        .push_chunk(err_chunk)
        .expect_err("must report error");
    assert!(matches!(err_res, ForwardError::UpstreamError(_)));
    assert!(!err_buf.committed);

    // 6. Buffer reaching 64 KiB without error auto-commits
    let mut big_buf = PreFirstFrameBuffer::new(StatusCode::OK, HeaderMap::new());
    let big_chunk = vec![b'x'; HOLD_BUFFER_MAX_BYTES];
    let big_res = big_buf.push_chunk(&big_chunk).expect("push 64kb");
    assert!(big_res.is_some());
    assert!(big_buf.committed);
}

/// Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2
/// Verify that when strip_injected_usage_chunk == true, only complete frames matching
/// choices:[] and an object usage are suppressed, and partial bytes at stream end flush verbatim.
#[doc = "Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2"]
#[test]
fn test_sse_forwarder_injected_usage_chunk_filtering() {
    let mut forwarder = SseChunkForwarder::new(true);

    // Normal chunk 1: forwarded
    let c1 = b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n";
    let out1 = forwarder.process_chunk(c1);
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].as_ref(), c1);

    // Injected usage chunk: suppressed!
    let usage_chunk =
        b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n";
    let out_usage = forwarder.process_chunk(usage_chunk);
    assert!(
        out_usage.is_empty(),
        "injected usage chunk must be suppressed"
    );

    // Normal chunk 2 split across two pushes
    let c2_part1 = b"data: {\"choices\":[{\"delta\":";
    let c2_part2 = b"{\"content\":\"second\"}}]}\n\n";

    let out_p1 = forwarder.process_chunk(c2_part1);
    assert!(out_p1.is_empty(), "partial frame must be buffered");

    let out_p2 = forwarder.process_chunk(c2_part2);
    assert_eq!(out_p2.len(), 1);
    let full_c2: Vec<u8> = [c2_part1.as_slice(), c2_part2.as_slice()].concat();
    assert_eq!(out_p2[0].as_ref(), full_c2.as_slice());

    // Partial tail at stream end flushes verbatim
    let tail = b"partial tail without delimiter";
    let out_tail = forwarder.process_chunk(tail);
    assert!(out_tail.is_empty());
    let finished = forwarder.finish().expect("flush remaining tail");
    assert_eq!(finished.as_ref(), tail);
}

/// Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2
/// Verify transparent fallback decoding for gzip, deflate, br, and zstd.
#[doc = "Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2"]
#[test]
fn test_compression_fallback_decoder() {
    let plain = b"data: {\"choices\":[{\"delta\":{\"content\":\"compressed content\"}}]}\n\n";

    // 1. Gzip compression and decompression
    let mut gz_encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz_encoder, plain).expect("write gz");
    let gz_bytes = gz_encoder.finish().expect("finish gz");
    let gz_decompressed = decompress_chunk("gzip", &gz_bytes).expect("decompress gz");
    assert_eq!(gz_decompressed.as_slice(), plain);

    // 2. Deflate compression and decompression
    let mut def_encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut def_encoder, plain).expect("write def");
    let def_bytes = def_encoder.finish().expect("finish def");
    let def_decompressed = decompress_chunk("deflate", &def_bytes).expect("decompress def");
    assert_eq!(def_decompressed.as_slice(), plain);

    // 3. Zstd compression and decompression
    let zstd_bytes = zstd::encode_all(plain.as_slice(), 3).expect("encode zstd");
    let zstd_decompressed = decompress_chunk("zstd", &zstd_bytes).expect("decompress zstd");
    assert_eq!(zstd_decompressed.as_slice(), plain);

    // 4. Brotli compression and decompression
    let mut br_bytes = Vec::new();
    {
        let mut br_writer = brotli::CompressorWriter::new(&mut br_bytes, 4096, 5, 20);
        std::io::Write::write_all(&mut br_writer, plain).expect("write br");
    }
    let br_decompressed = decompress_chunk("br", &br_bytes).expect("decompress br");
    assert_eq!(br_decompressed.as_slice(), plain);
}
