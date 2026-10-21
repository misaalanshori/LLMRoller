#![no_main]
//! Fuzz target for surgical JSON body editor and top-level scanning.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits
//! Phase: ./docs/bigbang/big-bang-phases-2.md#24-sub-phase-24-zero-copy-surgical-json-span-editor-duplicate-key-guard-and-body-edit-fuzzer

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use llmroller_core::rbac::PoolTier;
use llmroller_proto::body_edit::{ChatStreamMode, edit_body, scan_top_level};

fuzz_target!(|data: &[u8]| {
    if let Ok(_scan) = scan_top_level(data) {
        let b = Bytes::copy_from_slice(data);
        let _ = edit_body(
            b,
            PoolTier::Own,
            Some("target-model"),
            ChatStreamMode::Inject,
            Some(("session_id", "token123")),
        );
    }
});
