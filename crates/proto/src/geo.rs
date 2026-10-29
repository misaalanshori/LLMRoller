#![forbid(unsafe_code)]
//! Geo attribution from a trusted edge (`#158`, `USG-9`, `SEC-6`).
//!
//! `usage_events.client_location` is an attribution field that feeds audit and abuse review.
//! Before `#158` it was populated from the **inbound client's own headers**, which meant a
//! caller could write any value it liked into its own record. The field is now supplied only
//! when the connecting peer is a proxy the operator configured in
//! `LLMROLLER_TRUSTED_PROXIES`, and the decision is made here, at admission, where the peer
//! address is still known.
//!
//! Two rules govern the output, and both exist to stop the field from saying something that is
//! not true:
//!
//! - **Never invent a value.** Absent geo headers yield `None`, and the drawer renders a dash.
//!   The proxy does not synthesise a country from an IP, because it has no database that would
//!   make that a fact.
//! - **Drop, do not truncate.** A value that fails validation is discarded whole. A truncated
//!   country code is a different, still-wrong country, which is worse than a blank.
//!
//! Spec: ./docs/srdd/11. SRDD - Session Identification and Hints.md#113-session--client-environment-telemetry-in-the-request-detail-drawer-usg-9
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use http::HeaderMap;

/// Maximum length of one stored location component, in bytes.
///
/// The `usage_events.client_location` column has always been capped at 128 bytes; a city name
/// plus a country code fits in 64 comfortably, and bounding each component keeps the composed
/// `"{city}, {country}"` under the cap without ever truncating.
pub const MAX_LOCATION_COMPONENT_LEN: usize = 64;

/// Geo headers a trusted edge may assert, in the order they are consulted (`#158`).
///
/// `cf-ipcountry` is what Cloudflare sends; `x-geo-country` / `x-geo-city` are the generic
/// names (nginx `geoip2`, Traefik plugins). Only the first present value of each kind is used.
pub const TRUSTED_GEO_COUNTRY_HEADERS: [&str; 2] = ["cf-ipcountry", "x-geo-country"];
/// City header asserted by a trusted edge (`#158`).
pub const TRUSTED_GEO_CITY_HEADER: &str = "x-geo-city";

/// Every geo header this module reads, for the unconditional strip list and for tests.
pub const GEO_HEADERS: [&str; 3] = ["cf-ipcountry", "x-geo-country", "x-geo-city"];

/// A single validated geo component, or `None` when absent or unacceptable.
///
/// Accepts only printable ASCII up to [`MAX_LOCATION_COMPONENT_LEN`]. The narrowness is
/// deliberate: these values are rendered in an operator-facing drawer and stored in a column
/// that has historically been a plain `TEXT`. Anything that is not plain printable ASCII is
/// dropped rather than escaped, so no value from this path can need escaping at all.
fn validate_component(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() || value.len() > MAX_LOCATION_COMPONENT_LEN {
        return None;
    }
    if !value.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
        return None;
    }
    Some(value.to_string())
}

/// Compose the `client_location` value from already-validated parts.
///
/// Documented formats (`11.3`) are `"{city}, {country}"` and `"{country}"`. A city with no
/// country is rendered alone rather than as `"{city}, "` — a trailing separator would assert a
/// country that was not sent.
#[must_use]
pub fn compose_client_location(city: Option<&str>, country: Option<&str>) -> Option<String> {
    match (city, country) {
        (Some(city), Some(country)) => Some(format!("{city}, {country}")),
        (None, Some(country)) => Some(country.to_string()),
        (Some(city), None) => Some(city.to_string()),
        (None, None) => None,
    }
}

/// Derive `client_location` from inbound headers, but **only** for a trusted edge (`#158`).
///
/// `peer_trusted` is the admission-time answer from
/// [`llmroller_core::trust::peer_is_trusted_proxy`]; it is the caller's job to compute it while
/// the peer address is still available. When it is `false` this function returns `None` without
/// reading a single header, so an untrusted caller cannot influence the result by any means.
///
/// Use this only where the peer is known. The write-behind actor that persists the row cannot
/// see the connection, which is exactly why the value is decided here and carried to it.
#[must_use]
pub fn derive_client_location(headers: &HeaderMap, peer_trusted: bool) -> Option<String> {
    if !peer_trusted {
        return None;
    }
    let country = TRUSTED_GEO_COUNTRY_HEADERS
        .iter()
        .find_map(|name| validate_component(header_str(headers, name)));
    let city = validate_component(header_str(headers, TRUSTED_GEO_CITY_HEADER));
    compose_client_location(city.as_deref(), country.as_deref())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The `metadata_json` document carrying the admission-decided location to the writer.
///
/// The writer prefers this over any header it can see, so the trust decision is made once, at
/// admission, and travels with the event instead of being re-derived from client-controlled
/// bytes later (`#158`).
#[must_use]
pub fn client_location_metadata(location: Option<&str>) -> Option<String> {
    let location = location?;
    serde_json::to_string(&serde_json::json!({ "client_location": location })).ok()
}

#[cfg(test)]
mod tests;
