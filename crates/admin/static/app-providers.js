// LLMRoller Web UI — providers
// Provider creation, upstream key creation, and kind/preset handling.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

function onProviderKindChange(kind, selectId) {
    const isGlobal = selectId === 'gp-kind';
    const urlInput = document.getElementById(isGlobal ? 'gp-url' : 'prov-url');
    if (!urlInput) return;
    const selectEl = document.getElementById(selectId);
    const selectedOpt = selectEl && selectEl.selectedOptions ? selectEl.selectedOptions[0] : null;
    const dataUrl = selectedOpt ? selectedOpt.getAttribute('data-base-url') : null;
    if (!isGlobal && selectId === 'prov-kind') {
        const presetSelect = document.getElementById('prov-preset-id');
        if (presetSelect) {
            const presetAttr = selectedOpt ? selectedOpt.getAttribute('data-preset-id') : null;
            presetSelect.value = presetAttr || '';
        }
    }
    if (dataUrl !== null && dataUrl !== undefined && (dataUrl !== '' || kind === 'custom')) {
        urlInput.value = dataUrl;
        return;
    }
    if (kind === 'openrouter') urlInput.value = 'https://openrouter.ai/api/v1';
    else if (kind === 'opencode_go') urlInput.value = 'https://opencode.ai/zen/go/v1';
    else if (kind === 'command_code') urlInput.value = 'https://api.commandcode.ai/provider/v1';
    else if (kind === 'open_design_amr') urlInput.value = 'https://amr-link.open-design.ai/v1';
    else if (kind === 'custom') urlInput.value = '';
}

// ---------------------------------------------------------------------------
// Global Event Delegation (Zero inline event handlers)
// ---------------------------------------------------------------------------
const CORE_PROVIDER_KINDS = new Set([
    'openrouter',
    'opencode_go',
    'command_code',
    'open_design_amr',
    'custom'
]);

async function handleCreateProvider(e) {
    e.preventDefault();
    const name = document.getElementById('prov-name').value.trim();
    const selectedKind = document.getElementById('prov-kind').value;
    const presetOverride = document.getElementById('prov-preset-id')?.value || '';
    const base_url = document.getElementById('prov-url').value.trim();
    const allow_private_network = document.getElementById('prov-private-net').checked;

    const isCore = CORE_PROVIDER_KINDS.has(selectedKind);
    const preset_id = presetOverride || (!isCore ? selectedKind : null);
    const kind = preset_id ? 'custom' : selectedKind;

    try {
        const res = await fetch('/api/user/providers', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({
                name,
                kind,
                preset_id,
                base_url,
                config_json: '{}',
                allow_private_network
            })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-provider');
        document.getElementById('form-new-provider').reset();
        onProviderKindChange(document.getElementById('prov-kind').value, 'prov-kind');
        await invalidateAndRefetch('providers');
    } catch (err) {
        alert('Failed to add provider: ' + err);
    }
}

async function handleCreateUpstreamKey(e) {
    e.preventDefault();
    const provider_id = document.getElementById('target-provider-id').value;
    const label = document.getElementById('uk-label').value.trim();
    const secret = document.getElementById('uk-secret').value.trim();
    const prioVal = document.getElementById('uk-priority').value;
    const tierVal = document.getElementById('uk-tier').value;

    const payload = {
        label,
        secret,
        priority: prioVal ? parseInt(prioVal, 10) : 100,
        strict_tier: tierVal ? parseInt(tierVal, 10) : 1
    };

    const apiBase = window.location.pathname.startsWith('/admin') ? '/admin/api' : '/api/user';
    try {
        const res = await fetch(`${apiBase}/providers/${provider_id}/keys`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify(payload)
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-upstream-key');
        document.getElementById('form-new-upstream-key').reset();
        await invalidateAndRefetch('keys');
    } catch (err) {
        alert('Failed to add upstream key: ' + err);
    }
}

async function populateShareProviders() {
    const select = document.getElementById('share-provider-select');
    if (!select) return;
    select.replaceChildren();
    try {
        const res = await fetch('/api/user/providers');
        if (!res.ok) throw new Error('HTTP ' + res.status);
        const providers = await res.json();
        const own = (providers || []).filter(p => p.ownership_scope === 'personal' && p.enabled);
        if (own.length === 0) {
            const opt = document.createElement('option');
            opt.value = '';
            opt.textContent = 'No personal providers available';
            opt.disabled = true;
            select.appendChild(opt);
            return;
        }
        own.forEach(p => {
            const opt = document.createElement('option');
            opt.value = p.id;
            opt.textContent = p.name;
            select.appendChild(opt);
        });
    } catch (err) {
        const opt = document.createElement('option');
        opt.value = '';
        opt.textContent = 'Failed to load providers';
        opt.disabled = true;
        select.appendChild(opt);
    }
}

async function handleCreateGlobalProvider(e) {
    e.preventDefault();
    const name = document.getElementById('gp-name').value.trim();
    const selectedKind = document.getElementById('gp-kind').value;
    const base_url = document.getElementById('gp-url').value.trim();
    const allow_private_network = document.getElementById('gp-private-net').checked;

    const isCore = CORE_PROVIDER_KINDS.has(selectedKind);
    const preset_id = !isCore ? selectedKind : null;
    const kind = preset_id ? 'custom' : selectedKind;

    try {
        const res = await fetch('/admin/api/providers', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({
                name,
                kind,
                preset_id,
                base_url,
                config_json: '{}',
                allow_private_network
            })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-global-prov');
        document.getElementById('form-new-global-prov').reset();
        onProviderKindChange(document.getElementById('gp-kind').value, 'gp-kind');
        await invalidateAndRefetch('providers');
    } catch (err) {
        alert('Failed to create global provider: ' + err);
    }
}

async function testUpstreamKey(keyId) {
    const apiBase = window.location.pathname.startsWith('/admin') ? '/admin/api' : '/api/user';
    try {
        const res = await fetch(`${apiBase}/upstream-keys/${keyId}/test`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: '{}'
        });
        const data = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(data.error?.message || `HTTP ${res.status}`);
        alert(data.message || `Key probe succeeded (${data.discovered_models ?? 0} models discovered).`);
        await invalidateAndRefetch('keys');
    } catch (e) {
        alert('Key test failed: ' + e);
    }
}
