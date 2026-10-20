//! Unit tests for canonical AAD, HKDF subkeys, AEAD sealing, and master-key loading.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#922-canonical-aead-associated-data-aad-specification
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#164-component-mocking-and-dependency-injection-architecture
//! Phase: ./docs/bigbang/big-bang-phases-1.md#13-sub-phase-13-cryptographic-sealer-xchacha20-poly1305-31-table-sqlite-wal-store-and-resumable-key-rotation

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmroller_core::seams::{DeterministicRng, EntropySource, ExposeSecret};
use serde::Deserialize;

use super::{
    CryptoError, MasterKey, MasterKeyError, SecretSealer, XChaCha20Poly1305Sealer, canonical_aad,
    derive_subkey, info_label_for_domain, load_master_key, private_permissions_are_valid,
};

const SIX_DOMAINS: [&str; 6] = [
    "instance_secret",
    "totp_secret",
    "upstream_key",
    "usage_raw",
    "trace_block",
    "trace_record",
];

#[derive(Debug, Deserialize)]
struct Fixture {
    schema: u32,
    master_key_base64: String,
    instance_id_hex: String,
    vectors: Vec<FixtureVector>,
}

#[derive(Debug, Deserialize)]
struct FixtureVector {
    domain: String,
    info_label: String,
    fields_hex: Vec<String>,
    nonce_hex: String,
    plaintext_utf8: String,
    aad_hex: String,
    subkey_hex: String,
    sealed_hex: String,
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("crypto")
        .join("aad_vectors.json")
}

fn decode_hex(encoded: &str) -> Vec<u8> {
    assert_eq!(encoded.len() % 2, 0, "hex input must have an even length");
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for offset in (0..encoded.len()).step_by(2) {
        let pair = &encoded[offset..offset + 2];
        bytes.push(u8::from_str_radix(pair, 16).expect("fixture must contain valid hex"));
    }
    bytes
}

fn fixed_key_byte(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}

fn load_fixture() -> Fixture {
    serde_json::from_str(
        &fs::read_to_string(fixture_path()).expect("committed AAD fixture must exist"),
    )
    .expect("committed AAD fixture must be valid JSON")
}

fn fixture_sealer(fixture: &Fixture) -> (MasterKey, [u8; 16], XChaCha20Poly1305Sealer) {
    let master = MasterKey::from_base64(&fixture.master_key_base64).expect("fixture master key");
    let instance_id: [u8; 16] = decode_hex(&fixture.instance_id_hex)
        .try_into()
        .expect("fixture instance id must be 16 bytes");
    let entropy: Arc<dyn EntropySource> = Arc::new(DeterministicRng::new(0xAAD));
    let sealer = XChaCha20Poly1305Sealer::new(master.clone(), instance_id, Arc::clone(&entropy));
    (master, instance_id, sealer)
}

#[test]
#[doc = "Spec: SEC-1, Partial: PRV-5, Partial: SEC-19"]
fn committed_vectors_match_aad_hkdf_subkeys_and_envelopes() {
    let fixture = load_fixture();
    assert_eq!(fixture.schema, 1);

    let (master, instance_id, sealer) = fixture_sealer(&fixture);
    let mut seen_domains = BTreeSet::new();

    for vector in fixture.vectors {
        let field_bytes: Vec<Vec<u8>> = vector
            .fields_hex
            .iter()
            .map(|field| decode_hex(field))
            .collect();
        let fields: Vec<&[u8]> = field_bytes.iter().map(Vec::as_slice).collect();

        assert_eq!(
            info_label_for_domain(&vector.domain).expect("known fixture domain"),
            vector.info_label
        );
        assert_eq!(
            canonical_aad(&vector.domain, &fields),
            decode_hex(&vector.aad_hex),
            "AAD mismatch for {}",
            vector.domain
        );

        let derived = derive_subkey(&master, &instance_id, &vector.domain)
            .expect("fixture domain must derive a subkey");
        assert_eq!(
            derived.expose_secret(),
            decode_hex(&vector.subkey_hex).as_slice(),
            "HKDF mismatch for {}",
            vector.domain
        );

        let nonce: [u8; 24] = decode_hex(&vector.nonce_hex)
            .try_into()
            .expect("fixture nonce must be 24 bytes");
        let sealed = sealer
            .seal_with_nonce(
                &vector.domain,
                &fields,
                vector.plaintext_utf8.as_bytes(),
                nonce,
            )
            .expect("fixture domain must seal");
        assert_eq!(
            sealed,
            decode_hex(&vector.sealed_hex),
            "envelope mismatch for {}",
            vector.domain
        );

        let opened = sealer
            .open(&vector.domain, &fields, &sealed)
            .expect("fixture envelope must open");
        assert_eq!(
            opened.expose_secret(),
            vector.plaintext_utf8.as_bytes(),
            "plaintext mismatch for {}",
            vector.domain
        );

        assert!(
            seen_domains.insert(vector.domain),
            "fixture must not duplicate domains"
        );
    }

    let expected: BTreeSet<String> = SIX_DOMAINS
        .iter()
        .map(|domain| (*domain).to_owned())
        .collect();
    assert_eq!(seen_domains, expected, "fixture must cover all six domains");
}

#[test]
#[doc = "Spec: SEC-1, Partial: PRV-5, Partial: SEC-19"]
fn sealer_roundtrips_every_domain_and_generates_unique_nonces() {
    let fixture = load_fixture();
    let (_, _, sealer) = fixture_sealer(&fixture);

    let fields: [&[u8]; 2] = [b"mk_default", b"01ARZ3NDEKTSV4RRFFQ69G5FAV"];
    for domain in SIX_DOMAINS {
        let plaintext = format!("secret payload for {domain}");
        let first = sealer.seal(domain, &fields, plaintext.as_bytes());
        let second = sealer.seal(domain, &fields, plaintext.as_bytes());

        assert_eq!(first[0], 0x01, "envelope version must be 0x01");
        assert_eq!(first.len(), 1 + 24 + plaintext.len() + 16);
        assert_ne!(
            first, second,
            "fresh nonces must produce distinct envelopes"
        );

        let opened = sealer
            .open(domain, &fields, &first)
            .expect("sealed envelope must open");
        assert_eq!(opened.expose_secret(), plaintext.as_bytes());

        let empty = sealer.seal(domain, &fields, b"");
        assert_eq!(empty.len(), 1 + 24 + 16);
        assert_eq!(
            sealer
                .open(domain, &fields, &empty)
                .expect("empty plaintext must open")
                .expose_secret(),
            b""
        );
    }
}

#[test]
#[doc = "Spec: SEC-1"]
fn master_key_or_instance_id_changes_rerandomize_every_domain() {
    let fixture = load_fixture();
    let (master, instance_id, original) = fixture_sealer(&fixture);
    let entropy: Arc<dyn EntropySource> = Arc::new(DeterministicRng::new(0x1234));
    let rotated = XChaCha20Poly1305Sealer::new(
        MasterKey::from_base64(&fixed_key_byte(0x77)).expect("replacement master key"),
        instance_id,
        Arc::clone(&entropy),
    );
    let resalted = XChaCha20Poly1305Sealer::new(master, [0xAA; 16], Arc::clone(&entropy));

    let fields: [&[u8]; 2] = [b"mk_default", b"row-1"];
    let nonce = [0x42_u8; 24];
    for domain in SIX_DOMAINS {
        let plaintext = b"same plaintext under every key";
        let baseline = original
            .seal_with_nonce(domain, &fields, plaintext, nonce)
            .expect("baseline seal");
        let rotated_envelope = rotated
            .seal_with_nonce(domain, &fields, plaintext, nonce)
            .expect("rotated seal");
        let resalted_envelope = resalted
            .seal_with_nonce(domain, &fields, plaintext, nonce)
            .expect("resalted seal");

        assert_ne!(
            baseline, rotated_envelope,
            "master-key rotation must change {domain}"
        );
        assert_ne!(
            baseline, resalted_envelope,
            "instance-id change must change {domain}"
        );
        assert_ne!(
            rotated_envelope, resalted_envelope,
            "rotated and resalted domains must differ for {domain}"
        );
    }
}

#[test]
#[doc = "Spec: SEC-1, Partial: PRV-5"]
fn swapped_tampered_and_truncated_envelopes_are_rejected() {
    let fixture = load_fixture();
    let (_, _, sealer) = fixture_sealer(&fixture);

    let nonce = [0x5A_u8; 24];
    let fields_a: [&[u8]; 3] = [b"mk_default", b"key-a", b"provider-a"];
    let fields_b: [&[u8]; 3] = [b"mk_default", b"key-b", b"provider-a"];
    let sealed_a = sealer
        .seal_with_nonce("upstream_key", &fields_a, b"sk-canary-a", nonce)
        .expect("seal row A");
    let sealed_b = sealer
        .seal_with_nonce("upstream_key", &fields_b, b"sk-canary-a", nonce)
        .expect("seal row B");

    assert!(matches!(
        sealer.open("upstream_key", &fields_b, &sealed_a),
        Err(CryptoError::Authentication)
    ));
    assert!(matches!(
        sealer.open("usage_raw", &fields_a, &sealed_a),
        Err(CryptoError::Authentication)
    ));
    assert!(matches!(
        sealer.open("upstream_key", &fields_a, &sealed_b),
        Err(CryptoError::Authentication)
    ));

    let mut wrong_nonce = sealed_a.clone();
    wrong_nonce[1] ^= 0x01;
    assert!(matches!(
        sealer.open("upstream_key", &fields_a, &wrong_nonce),
        Err(CryptoError::Authentication)
    ));

    let mut wrong_version = sealed_a.clone();
    wrong_version[0] = 0x02;
    assert!(matches!(
        sealer.open("upstream_key", &fields_a, &wrong_version),
        Err(CryptoError::UnsupportedVersion(2))
    ));

    assert!(matches!(
        sealer.open("upstream_key", &fields_a, &sealed_a[..40]),
        Err(CryptoError::TruncatedEnvelope)
    ));

    let mut bit_flip = sealed_a.clone();
    if let Some(last) = bit_flip.last_mut() {
        *last ^= 0x01;
    }
    assert!(matches!(
        sealer.open("upstream_key", &fields_a, &bit_flip),
        Err(CryptoError::Authentication)
    ));

    assert!(matches!(
        sealer.open("not_a_domain", &fields_a, &sealed_a),
        Err(CryptoError::UnknownDomain(_))
    ));
}

#[test]
#[doc = "Spec: SEC-1"]
fn canonical_aad_length_prefixes_prevent_field_boundary_collisions() {
    let left = canonical_aad("upstream_key", &[b"ab".as_slice(), b"c".as_slice()]);
    let right = canonical_aad("upstream_key", &[b"a".as_slice(), b"bc".as_slice()]);
    assert_ne!(left, right);

    let with_nul = canonical_aad("upstream_key", &[b"a\0b".as_slice(), b"c".as_slice()]);
    assert_ne!(left, with_nul);

    let domain_a = canonical_aad("trace_record", &[b"same"]);
    let domain_b = canonical_aad("trace_block", &[b"same"]);
    assert_ne!(domain_a, domain_b);

    assert!(left.starts_with(b"llmroller.aad.v1\0"));
}

#[test]
#[doc = "Spec: SEC-2, Partial: PRV-5"]
fn master_key_requires_base64_encoding_of_exactly_32_bytes() {
    let valid = MasterKey::from_base64(&fixed_key_byte(0x11)).expect("32-byte key must load");
    assert_eq!(valid.as_bytes(), &[0x11; 32]);

    let thirty_one = MasterKey::from_base64(&STANDARD.encode([0x22_u8; 31]));
    assert!(matches!(thirty_one, Err(MasterKeyError::InvalidLength(31))));

    let thirty_three = MasterKey::from_base64(&STANDARD.encode([0x22_u8; 33]));
    assert!(matches!(
        thirty_three,
        Err(MasterKeyError::InvalidLength(33))
    ));

    assert!(matches!(
        MasterKey::from_base64("not base64 !!!"),
        Err(MasterKeyError::InvalidBase64)
    ));
    assert_eq!(
        MasterKeyError::InvalidLength(31).exit_code(),
        3,
        "SEC-2 requires startup exit code 3"
    );
}

#[test]
#[doc = "Spec: SEC-2, SEC-14"]
fn master_key_debug_output_never_reveals_material() {
    let key = MasterKey::from_bytes([0x41; 32]);
    let debug = format!("{key:?}");
    assert!(
        debug.contains("REDACTED"),
        "Debug must mark redaction: {debug}"
    );
    assert!(
        !debug.contains("QUFB"),
        "Debug must not reveal base64 key material: {debug}"
    );
}

#[test]
#[doc = "Spec: SEC-2"]
fn private_permission_modes_reject_group_or_other_bits() {
    for mode in [0o600, 0o400, 0o700, 0o500, 0o000] {
        assert!(
            private_permissions_are_valid(mode),
            "mode {mode:o} is private"
        );
    }
    for mode in [0o640, 0o604, 0o660, 0o606, 0o777, 0o001, 0o010] {
        assert!(
            !private_permissions_are_valid(mode),
            "mode {mode:o} exposes the key to group or other"
        );
    }
}

#[test]
#[doc = "Spec: SEC-2"]
fn master_key_loading_prefers_the_file_and_trims_a_trailing_newline() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("master.key");
    fs::write(&path, format!("{}\n", fixed_key_byte(0x33))).expect("write key file");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod key file");
    }

    let from_file = load_master_key(Some(&path), Some(&fixed_key_byte(0x44)))
        .expect("file must win over inline value");
    assert_eq!(from_file.as_bytes(), &[0x33; 32]);

    let from_inline =
        load_master_key(None, Some(&fixed_key_byte(0x44))).expect("inline key must load");
    assert_eq!(from_inline.as_bytes(), &[0x44; 32]);

    assert!(matches!(
        load_master_key(None, None),
        Err(MasterKeyError::Missing)
    ));
}

#[test]
#[cfg(unix)]
#[doc = "Spec: SEC-2"]
fn master_key_file_rejects_group_or_other_permission_bits() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("master.key");
    fs::write(&path, fixed_key_byte(0x55)).expect("write key file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod key file");

    assert!(matches!(
        load_master_key(Some(&path), None),
        Err(MasterKeyError::InsecurePermissions)
    ));
}

#[test]
#[doc = "Spec: SEC-2"]
fn missing_key_file_is_reported_instead_of_falling_back() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("does-not-exist.key");

    assert!(matches!(
        load_master_key(Some(&missing), Some(&fixed_key_byte(0x66))),
        Err(MasterKeyError::Io { .. })
    ));
}
