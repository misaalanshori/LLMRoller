// LLMRoller Web UI — shares
// Consented personal sharing: create and accept.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

async function handleCreateShare(e) {
    e.preventDefault();
    const grantee_user_id = document.getElementById('share-grantee').value.trim();
    const provider_id = document.getElementById('share-provider-select').value;
    const capVal = document.getElementById('share-cap').value;
    const modelsVal = document.getElementById('share-models').value.trim();

    if (!provider_id) {
        alert('Select a personal provider first.');
        return;
    }

    const payload = {
        grantee_user_id,
        provider_id,
        upstream_key_id: null,
        max_monthly_micro_usd: capVal ? Math.round(parseFloat(capVal) * 1000000) : null,
        allowed_models: modelsVal ? modelsVal.split(',').map(s => s.trim()) : null
    };

    try {
        const res = await fetch('/api/user/shares', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify(payload)
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-share');
        document.getElementById('form-new-share').reset();
        await invalidateAndRefetch('shares');
    } catch (err) {
        alert('Failed to grant share: ' + err);
    }
}

async function acceptShare(id, fingerprint) {
    if (!id) return;
    try {
        const res = await fetch(`/api/user/shares/${encodeURIComponent(id)}/accept`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ config_fingerprint: fingerprint, fingerprint })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('shares');
    } catch (err) {
        alert('Failed to accept share: ' + err);
    }
}
