//! Geo attribution tests (`#158`, `USG-9`, `SEC-6`).
//!
//! These test the derivation rule, not the HTTP server. The trust decision itself is tested in
//! `llmroller_core::trust`; here it is an input.

use super::*;
use http::HeaderValue;
use http::header::HeaderName;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            HeaderValue::from_str(value).expect("valid header value"),
        );
    }
    map
}

// ---------------------------------------------------------------- the security rule (#158)

#[test]
fn an_untrusted_peer_cannot_set_location() {
    let map = headers(&[("cf-ipcountry", "ZZ")]);
    assert_eq!(derive_client_location(&map, false), None);
}

#[test]
fn an_untrusted_peer_cannot_set_location_by_any_geo_header() {
    let map = headers(&[
        ("cf-ipcountry", "ZZ"),
        ("x-geo-country", "ZZ"),
        ("x-geo-city", "Atlantis"),
    ]);
    assert_eq!(derive_client_location(&map, false), None);
}

#[test]
fn a_trusted_peer_may_supply_a_country() {
    let map = headers(&[("cf-ipcountry", "US")]);
    assert_eq!(derive_client_location(&map, true), Some("US".to_string()));
}

// ---------------------------------------------------------------- documented formats (11.3)

#[test]
fn x_geo_city_and_country_render_as_city_then_country() {
    let map = headers(&[("x-geo-country", "SG"), ("x-geo-city", "Singapore")]);
    assert_eq!(
        derive_client_location(&map, true),
        Some("Singapore, SG".to_string()),
        "11.3 documents the format as \"{{city}}, {{country}}\""
    );
}

#[test]
fn a_country_without_a_city_renders_as_the_country_alone() {
    let map = headers(&[("x-geo-country", "SG")]);
    assert_eq!(derive_client_location(&map, true), Some("SG".to_string()));
}

#[test]
fn cf_ipcountry_wins_over_x_geo_country() {
    let map = headers(&[("cf-ipcountry", "US"), ("x-geo-country", "SG")]);
    assert_eq!(
        derive_client_location(&map, true),
        Some("US".to_string()),
        "the Cloudflare header is consulted first, deterministically"
    );
}

#[test]
fn x_geo_country_is_used_when_cf_ipcountry_is_absent() {
    let map = headers(&[("x-geo-country", "SG")]);
    assert_eq!(derive_client_location(&map, true), Some("SG".to_string()));
}

#[test]
fn a_city_without_a_country_renders_alone_not_with_a_trailing_comma() {
    let map = headers(&[("x-geo-city", "Singapore")]);
    assert_eq!(
        derive_client_location(&map, true),
        Some("Singapore".to_string()),
        "a trailing \", \" would assert a country that was not sent"
    );
}

#[test]
fn no_geo_header_yields_nothing() {
    let map = headers(&[("user-agent", "claude-cli/1.0")]);
    assert_eq!(
        derive_client_location(&map, true),
        None,
        "location must never be invented (cycle 174's rule)"
    );
}

// ---------------------------------------------------------------- malformed values

#[test]
fn an_oversized_value_is_dropped_not_truncated() {
    let long = "A".repeat(MAX_LOCATION_COMPONENT_LEN + 1);
    let map = headers(&[("x-geo-city", &long)]);
    assert_eq!(
        derive_client_location(&map, true),
        None,
        "a truncated city is a different city; dropping is the honest choice"
    );
}

#[test]
fn a_value_at_the_length_limit_is_kept() {
    let exact = "A".repeat(MAX_LOCATION_COMPONENT_LEN);
    let map = headers(&[("x-geo-city", &exact)]);
    assert_eq!(derive_client_location(&map, true), Some(exact));
}

#[test]
fn a_non_ascii_value_is_dropped() {
    let map = headers(&[("x-geo-city", "Sïngäpöré")]);
    assert_eq!(derive_client_location(&map, true), None);
}

#[test]
fn a_control_character_is_dropped() {
    // A tab is legal inside a HeaderValue, so this reaches the validator rather than being
    // rejected by the header parser first.
    let map = headers(&[("x-geo-city", "Sing\tapore")]);
    assert_eq!(derive_client_location(&map, true), None);
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    let map = headers(&[("x-geo-country", "  SG  ")]);
    assert_eq!(derive_client_location(&map, true), Some("SG".to_string()));
}

#[test]
fn an_empty_value_yields_nothing() {
    let map = headers(&[("cf-ipcountry", ""), ("x-geo-city", "   ")]);
    assert_eq!(derive_client_location(&map, true), None);
}

#[test]
fn a_malformed_city_does_not_discard_a_valid_country() {
    let long = "A".repeat(MAX_LOCATION_COMPONENT_LEN + 1);
    let map = headers(&[("x-geo-country", "SG"), ("x-geo-city", &long)]);
    assert_eq!(derive_client_location(&map, true), Some("SG".to_string()));
}

// ---------------------------------------------------------------- composition

#[test]
fn compose_returns_none_when_both_parts_are_absent() {
    assert_eq!(compose_client_location(None, None), None);
}

#[test]
fn compose_pairs_city_and_country_with_a_comma_space() {
    assert_eq!(
        compose_client_location(Some("Singapore"), Some("SG")).as_deref(),
        Some("Singapore, SG")
    );
}

// ---------------------------------------------------------------- metadata carrier

#[test]
fn the_metadata_carrier_round_trips_the_location() {
    let carrier = client_location_metadata(Some("Singapore, SG")).expect("carrier");
    let parsed: serde_json::Value = serde_json::from_str(&carrier).expect("valid JSON");
    assert_eq!(parsed["client_location"], "Singapore, SG");
}

#[test]
fn an_absent_location_produces_no_carrier() {
    assert_eq!(
        client_location_metadata(None),
        None,
        "absent must stay absent all the way to the column, not become a JSON null"
    );
}
