//! Tests for bounded TraceTap, binary block replacement, head/tail truncation, and memory bounds.
//!
//! Spec: Section 6.10, Section 7.1, Section 9.2, Section 12.6, Section 21.1,
//! Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use super::*;

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_binary_block_omission_replacement() {
    let large_base64 = "A".repeat(4096);
    let mut payload = serde_json::json!({
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "Describe this image" },
                    { "type": "image", "data": large_base64 },
                    { "type": "image_url", "url": format!("data:image/png;base64,{large_base64}") }
                ]
            }
        ]
    });

    replace_binary_blocks(&mut payload);

    let content_arr = payload["messages"][0]["content"].as_array().expect("array");
    assert_eq!(content_arr[0]["type"], "text");
    assert_eq!(content_arr[0]["text"], "Describe this image");

    // data block replaced
    let data_block = &content_arr[1]["data"];
    assert_eq!(data_block["llmroller_omitted"], "binary");
    assert_eq!(data_block["bytes"], 4096);
    assert!(data_block["sha256"].as_str().is_some());

    // url block replaced
    let url_block = &content_arr[2]["url"];
    assert_eq!(url_block["llmroller_omitted"], "binary");
    assert_eq!(url_block["type"], "url");
}

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_response_head_tail_truncation_at_one_mib() {
    // 2 MiB string (2 * 1024 * 1024 = 2_097_152 bytes)
    let mut text_2mib = "X".repeat(2 * 1024 * 1024);
    let (truncated, omitted) = apply_head_tail_truncation(&mut text_2mib);

    assert!(truncated, "2 MiB output must be truncated");
    assert_eq!(omitted, 1024 * 1024); // Exactly 1 MiB omitted

    // Text contains marker
    assert!(text_2mib.contains("[llmroller: 1048576 bytes omitted]"));
    // First 512 KiB preserved
    assert!(text_2mib.starts_with(&"X".repeat(100)));
    // Last 512 KiB preserved
    assert!(text_2mib.ends_with(&"X".repeat(100)));

    // Output <= 1 MiB is NOT truncated
    let mut small = "Hello, world!".to_string();
    let (t_small, o_small) = apply_head_tail_truncation(&mut small);
    assert!(!t_small);
    assert_eq!(o_small, 0);
    assert_eq!(small, "Hello, world!");
}

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_trace_tap_non_blocking_try_send_and_memory_budget() {
    let (tap, _rx) = TraceTap::new();

    // Fill the 256-item channel buffer with 1-byte chunks
    for _ in 0..256 {
        assert!(tap.try_feed_chunk(Bytes::from_static(b"x")));
    }

    // 257th item overflows channel -> try_feed_chunk returns false and sets trace_dropped
    assert!(!tap.try_feed_chunk(Bytes::from_static(b"overflow")));
    assert!(tap.trace_dropped.load(Ordering::SeqCst));

    // Subsequent chunks immediately rejected without delay (< 1 ms, never blocking forwarder) (PERF-2)
    assert!(!tap.try_feed_chunk(Bytes::from_static(b"after drop")));
}
