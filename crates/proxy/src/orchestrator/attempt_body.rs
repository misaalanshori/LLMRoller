//! Outbound request-body preparation for one attempt: session/stream field editing,
//! cross-key failover sanitisation, protocol transcoding, provider preset normalization,
//! OpenAI-compat and OpenRouter cache rewrites, and upstream header synthesis.
//!
//! Extracted verbatim from `pipeline.rs`'s attempt loop. This is a pure move: the
//! sequence of transformations and their order are unchanged, including the
//! `attempt_no > 1` failover sanitisation and the `body.clone()` on the edit path.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md
//! Spec: ./docs/srdd/24. SRDD - Cross-Protocol Transcoding and OpenAI Client Normalization.md

use super::*;

/// Immutable inputs the body-preparation step reads from the attempt loop.
pub(crate) struct PreservedAttemptFacts<'a> {
    pub(crate) body: &'a Bytes,
    pub(crate) candidate: &'a RouteCandidate,
    pub(crate) endpoint: EndpointKind,
    pub(crate) attempt_endpoint: EndpointKind,
    pub(crate) is_streaming: bool,
    pub(crate) attempt_no: usize,
    pub(crate) resolved_session: Option<&'a ResolvedSession>,
    pub(crate) headers: &'a HeaderMap,
    pub(crate) secret: &'a str,
    /// The loop's admission-timestamp read; reused by header synthesis so the value is
    /// identical to what the inline block closed over.
    pub(crate) now: i64,
    /// Inbound protocol name, used to decide whether transcoding applies.
    pub(crate) protocol: &'a str,
    /// `Some((src, dst))` when this candidate needs cross-protocol transcoding.
    pub(crate) transcode_pair: Option<(
        llmroller_proto::transcode::WireProtocol,
        llmroller_proto::transcode::WireProtocol,
    )>,
    /// Resolved `previous_response_id` link, if the request continues one. Its presence
    /// suppresses the failover sanitisation below, so the `Option` shape is load-bearing.
    pub(crate) hard_link: Option<ResponseLinkRow>,
    /// Fully synthesized upstream URL, needed only to detect the Command Code envelope.
    pub(crate) target_url: &'a str,
}

/// The prepared outbound request pieces.
pub(crate) struct PreparedAttemptBody {
    pub(crate) edited_body: Bytes,
    pub(crate) outbound_headers: HeaderMap,
    pub(crate) strip_injected_usage: bool,
}

/// Apply every body and header transformation for one attempt.
///
/// Returns `Err` when header synthesis fails, carrying the response the caller must
/// return verbatim — the same early return the inline block performed.
pub(crate) fn prepare_attempt_body(
    facts: PreservedAttemptFacts<'_>,
) -> Result<PreparedAttemptBody, Box<Response>> {
    let PreservedAttemptFacts {
        body,
        candidate,
        endpoint,
        attempt_endpoint,
        is_streaming,
        attempt_no,
        resolved_session,
        headers,
        secret,
        now,
        protocol,
        transcode_pair,
        hard_link,
        target_url,
    } = facts;
    // Only Chat Completions understands `stream_options`; native Messages and
    // Responses streams must pass through byte-identically (`API-4`, `TRN-2`).
    let chat_stream_mode = if is_streaming && endpoint == EndpointKind::Chat {
        candidate.config.usage_reporting.chat_stream.map_or_else(
            || get_adapter(candidate.provider_kind).default_chat_stream_mode(),
            ChatStreamMode::from,
        )
    } else {
        ChatStreamMode::Native
    };
    let session_profile = candidate.session_profile();
    let session_token = resolved_session
        .as_ref()
        .map(|session| session.outbound_token(candidate.pool_tier()));
    let session_field = session_profile
        .body_field
        .as_deref()
        .zip(session_token)
        .filter(|_| {
            get_adapter(candidate.provider_kind).supports_session_body_on_endpoint(endpoint)
        });
    let (mut edited_body, strip_injected_usage) = match edit_body_with_session_limit(
        body.clone(),
        candidate.pool_tier(),
        Some(&candidate.upstream_model_id),
        chat_stream_mode,
        session_field,
        session_profile.max_len,
    ) {
        Ok(edited) => edited,
        Err(_) => (body.clone(), false),
    };
    // F005: On failover across keys for /v1/responses, strip unpinned previous_response_id
    // and cross-account encrypted reasoning items so the backup key does not return 400.
    if attempt_no > 1
        && matches!(
            endpoint,
            EndpointKind::Responses | EndpointKind::ResponsesCompact
        )
    {
        edited_body = llmroller_proto::body_edit::sanitize_responses_failover_body(
            edited_body,
            hard_link.is_none(),
        );
    }
    // F099: If the candidate uses a different upstream wire protocol, transcode the request body.
    if let Some((src_wire, dst_wire)) = transcode_pair
        && let Ok(transcoded) = llmroller_proto::transcode::transcode_request(
            src_wire,
            dst_wire,
            &edited_body,
            &candidate.upstream_model_id,
        )
    {
        edited_body = Bytes::from(transcoded);
    }
    // F101-F180: Apply compiled-in provider preset payload normalizations when `preset_id` is configured.
    if let Some(ref preset_id) = candidate.config.preset_id
        && let Some(spec) = llmroller_proto::providers::lookup_provider_preset(preset_id)
    {
        edited_body = (spec.normalize_request_body)(edited_body, &candidate.upstream_model_id);
    }
    // F095 / FND-009: When targeting Command Code native /alpha/generate, wrap body in /alpha/generate envelope.
    if candidate.provider_kind == ProviderKind::CommandCode
        && target_url.ends_with("/alpha/generate")
        && transcode_pair.is_none()
        && let Some(src_wire) = llmroller_proto::transcode::WireProtocol::from_catalog_str(protocol)
    {
        let cc_thread = resolved_session
            .as_ref()
            .map(|s| s.outbound_token(candidate.pool_tier()))
            .unwrap_or("thr_llmroller");
        if let Ok(env) = llmroller_proto::providers::command_code::build_alpha_generate_envelope(
            src_wire,
            &edited_body,
            &candidate.upstream_model_id,
            cc_thread,
        ) {
            edited_body = Bytes::from(env);
        }
    }
    // F096: On OpenCode Go /v1/responses, force `store: false` and strip prior `reasoning` items.
    if candidate.provider_kind == ProviderKind::OpencodeGo
        && attempt_endpoint == EndpointKind::Responses
    {
        edited_body = llmroller_proto::body_edit::sanitize_opencode_go_responses_body(edited_body);
    }
    // F007, F024, F098: Apply provider OpenAI Chat Completions compatibility normalizations,
    // OpenRouter Anthropic prompt cache breakpoint injection, and declarative body_edits.
    let preset_compat = candidate
        .config
        .preset_id
        .as_deref()
        .and_then(llmroller_proto::providers::lookup_provider_preset)
        .and_then(|spec| (spec.default_provider_config)().openai_compat);
    let effective_compat = candidate
        .config
        .openai_compat
        .as_ref()
        .or(preset_compat.as_ref());
    let has_compat = attempt_endpoint == EndpointKind::Chat && effective_compat.is_some();
    let has_or_cache = attempt_endpoint == EndpointKind::Chat
        && candidate.provider_kind == ProviderKind::Openrouter;
    let has_rules = !candidate.config.body_edits.is_empty();
    if (has_compat || has_or_cache || has_rules)
        && let Ok(mut json_doc) = serde_json::from_slice::<serde_json::Value>(&edited_body)
    {
        let mut doc_changed = false;
        if let Some(compat) = effective_compat
            && attempt_endpoint == EndpointKind::Chat
            && llmroller_proto::body_edit::apply_openai_completions_compat(&mut json_doc, compat)
        {
            doc_changed = true;
        }
        if has_or_cache
            && llmroller_proto::body_edit::inject_openrouter_anthropic_cache_control(
                &mut json_doc,
                &candidate.upstream_model_id,
            )
        {
            doc_changed = true;
        }
        if has_rules
            && llmroller_proto::body_edit::apply_body_edit_rules(
                &mut json_doc,
                &candidate.config.body_edits,
            )
        {
            doc_changed = true;
        }
        if doc_changed && let Ok(serialized) = serde_json::to_vec(&json_doc) {
            edited_body = Bytes::from(serialized);
        }
    }

    let outbound_headers = build_upstream_headers(
        headers,
        candidate,
        secret,
        attempt_endpoint,
        resolved_session,
        now,
    )?;

    Ok(PreparedAttemptBody {
        edited_body,
        outbound_headers,
        strip_injected_usage,
    })
}
