-- LLMRoller schema version 2: persist the discovered model context window.
--
-- Spec: docs/srdd/9. SRDD - Data Model.md Section 9.1 (`models`)
-- Spec: MDL-1 (discovery) and API-1 (`GET /v1/models` capability projection)
--
-- NULL means "the upstream never advertised a context length". The proxy must
-- omit the field rather than fabricate a capability value (audit F-031).

ALTER TABLE models ADD COLUMN context_window INTEGER;
