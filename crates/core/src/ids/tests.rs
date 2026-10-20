//! Unit tests for ULID-backed domain identifiers.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#92-identifiers-and-ulid-conventions
//! Phase: ./docs/bigbang/big-bang-phases-1.md#12-sub-phase-12-core-domain-identifiers-actorscope-enum-and-seam-traits

use std::collections::BTreeSet;

use super::{
    ClientKeyId, IdParseError, ModelPk, ProviderId, RequestId, ShareId, UpstreamKeyId,
    UsageEventId, UserId,
};

const SAMPLE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SAMPLE_NEXT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

#[test]
#[doc = "Partial: OBS-1"]
fn ulid_wrappers_roundtrip_display_parse_and_serde() {
    macro_rules! check {
        ($ty:ident) => {{
            let id = $ty::parse(SAMPLE).expect("sample ULID must parse");
            assert_eq!(id.to_string(), SAMPLE);
            assert_eq!(id.as_ulid().to_string(), SAMPLE);

            let encoded = serde_json::to_string(&id).expect("ULID must serialize");
            assert_eq!(encoded, format!("\"{SAMPLE}\""));

            let decoded: $ty = serde_json::from_str(&encoded).expect("ULID must deserialize");
            assert_eq!(decoded, id);

            let via_from_str: $ty = SAMPLE
                .parse()
                .expect("FromStr must accept a canonical ULID");
            assert_eq!(via_from_str, id);
        }};
    }

    check!(UserId);
    check!(ProviderId);
    check!(UpstreamKeyId);
    check!(ClientKeyId);
    check!(ShareId);
    check!(ModelPk);
    check!(RequestId);
    check!(UsageEventId);
}

#[test]
#[doc = "Partial: OBS-1"]
fn generated_identifiers_are_unique_and_canonical() {
    let mut seen = BTreeSet::new();
    for _ in 0..512 {
        let id = UserId::generate();
        assert_eq!(
            id.to_string().len(),
            26,
            "ULIDs are always 26 Crockford characters"
        );
        assert!(seen.insert(id), "generated identifiers must be unique");
    }
}

#[test]
#[doc = "Partial: OBS-1"]
fn ulid_ordering_is_lexicographic() {
    let first = UserId::parse(SAMPLE).expect("sample ULID must parse");
    let second = UserId::parse(SAMPLE_NEXT).expect("next sample ULID must parse");

    assert!(first < second);
    assert!(first.to_string() < second.to_string());
}

#[test]
#[doc = "Partial: OBS-1"]
fn malformed_identifiers_are_rejected_for_every_wrapper() {
    macro_rules! check {
        ($ty:ident) => {{
            for invalid in [
                "",
                "not-a-ulid",
                "01ARZ3NDEKTSV4RRFFQ69G5FA",
                "01ARZ3NDEKTSV4RRFFQ69G5FAU",
            ] {
                let error: IdParseError = $ty::parse(invalid).expect_err("invalid ULID must fail");
                assert!(
                    error.to_string().contains("invalid ULID"),
                    "parse errors must be descriptive: {error}"
                );
            }
        }};
    }

    check!(UserId);
    check!(ProviderId);
    check!(UpstreamKeyId);
    check!(ClientKeyId);
    check!(ShareId);
    check!(ModelPk);
    check!(RequestId);
    check!(UsageEventId);
}
