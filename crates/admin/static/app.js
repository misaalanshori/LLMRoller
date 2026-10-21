// LLMRoller Web UI Vanilla JS Event Delegation & HTMX Integration
// All interactions use event delegation on document to strictly adhere to CSP: script-src 'self'

htmx.config.includeIndicatorStyles = false;
htmx.config.allowEval = false;
htmx.config.allowScriptTags = false;

function getCsrfToken() {
    const meta = document.querySelector('meta[name="csrf-token"]');
    return meta ? meta.getAttribute('content') : '';
}

function switchTab(tabId, btn) {
    const container = btn.closest('.workspace-layout');
    if (!container) return;
    container.querySelectorAll('.tab-btn').forEach(b => b.classList.remove('active'));
    container.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'));
    btn.classList.add('active');
    const target = document.getElementById(tabId);
    if (target) target.classList.add('active');
}

function openModal(id) {
    const modal = document.getElementById(id);
    if (modal) modal.classList.add('open');
}

function closeModal(id) {
    const modal = document.getElementById(id);
    if (modal) modal.classList.remove('open');
}

function copySnippet(id) {
    const el = document.getElementById(id);
    if (!el) return;
    const text = el.innerText || el.textContent;
    navigator.clipboard.writeText(text);
    alert('Copied to clipboard!');
}

function onProviderKindChange(kind, selectId) {
    const isGlobal = selectId === 'gp-kind';
    const urlInput = document.getElementById(isGlobal ? 'gp-url' : 'prov-url');
    if (!urlInput) return;
    if (kind === 'openrouter') urlInput.value = 'https://openrouter.ai/api/v1';
    else if (kind === 'opencode_go') urlInput.value = 'https://opencode.ai/zen/go/v1';
    else if (kind === 'command_code') urlInput.value = 'https://commandcode.ai/alpha';
    else if (kind === 'custom') urlInput.value = '';
}

// ---------------------------------------------------------------------------
// Global Event Delegation (Zero inline event handlers)
// ---------------------------------------------------------------------------
document.addEventListener('click', function(e) {
    // 1. Tab Navigation
    const tabBtn = e.target.closest('[data-tab]');
    if (tabBtn) {
        const tabId = tabBtn.getAttribute('data-tab');
        switchTab(tabId, tabBtn);
        return;
    }

    // 2. Modal Open
    const modalOpenBtn = e.target.closest('[data-modal-open]');
    if (modalOpenBtn) {
        const modalId = modalOpenBtn.getAttribute('data-modal-open');
        const provId = modalOpenBtn.getAttribute('data-provider-id');
        if (provId) {
            const targetInput = document.getElementById('target-provider-id');
            if (targetInput) targetInput.value = provId;
        }
        openModal(modalId);
        return;
    }

    // 3. Modal Close
    const modalCloseBtn = e.target.closest('[data-modal-close]');
    if (modalCloseBtn) {
        const modalId = modalCloseBtn.getAttribute('data-modal-close');
        closeModal(modalId);
        return;
    }

    // 4. Copy to Clipboard
    const copyBtn = e.target.closest('[data-copy]');
    if (copyBtn) {
        const snippetId = copyBtn.getAttribute('data-copy');
        copySnippet(snippetId);
        return;
    }

    // 5. Preview Chat
    const previewBtn = e.target.closest('[data-action="preview-chat"]');
    if (previewBtn) {
        const hash = previewBtn.getAttribute('data-session-hash');
        openChatPreview(hash);
        return;
    }

    // 6. Test Key Probe
    const testBtn = e.target.closest('[data-action="test-key"]');
    if (testBtn) {
        const keyId = testBtn.getAttribute('data-key-id');
        testUpstreamKey(keyId);
        return;
    }

    // 7. Toggle User Disabled State
    const toggleUserBtn = e.target.closest('[data-action="toggle-user"]');
    if (toggleUserBtn) {
        const uid = toggleUserBtn.getAttribute('data-id');
        const disabled = toggleUserBtn.getAttribute('data-disabled') === 'true';
        toggleUser(uid, disabled);
        return;
    }

    // 8. Toggle Model Exposed State
    const toggleModelBtn = e.target.closest('[data-action="toggle-model"]');
    if (toggleModelBtn) {
        const modelPk = toggleModelBtn.getAttribute('data-id');
        const exposed = parseInt(toggleModelBtn.getAttribute('data-exposed'), 10);
        toggleModelExposed(modelPk, exposed);
        return;
    }

    // 9. Save Setting
    const saveSettingBtn = e.target.closest('[data-action="save-setting"]');
    if (saveSettingBtn) {
        const key = saveSettingBtn.getAttribute('data-key');
        saveSetting(key);
        return;
    }
});

// ---------------------------------------------------------------------------
// Global Form Submissions
// ---------------------------------------------------------------------------
document.addEventListener('submit', function(e) {
    if (e.target.id === 'form-new-key') {
        handleCreateClientKey(e);
    } else if (e.target.id === 'form-new-provider') {
        handleCreateProvider(e);
    } else if (e.target.id === 'form-new-upstream-key') {
        handleCreateUpstreamKey(e);
    } else if (e.target.id === 'form-new-share') {
        handleCreateShare(e);
    } else if (e.target.id === 'form-new-user') {
        handleCreateUser(e);
    } else if (e.target.id === 'form-new-global-prov') {
        handleCreateGlobalProvider(e);
    } else if (e.target.id === 'form-new-alias') {
        handleAddAlias(e);
    }
});

// ---------------------------------------------------------------------------
// Global Change Listeners
// ---------------------------------------------------------------------------
document.addEventListener('change', function(e) {
    if (e.target.id === 'prov-kind' || e.target.id === 'gp-kind') {
        onProviderKindChange(e.target.value, e.target.id);
    }
});

// ---------------------------------------------------------------------------
// Async Handlers
// ---------------------------------------------------------------------------
async function handleCreateClientKey(e) {
    e.preventDefault();
    const label = document.getElementById('key-label').value.trim();
    const scope = document.getElementById('key-scope').value;
    const rpmVal = document.getElementById('key-rpm').value;
    const budgetVal = document.getElementById('key-budget').value;

    const payload = {
        label,
        routing_scope: scope,
        rate_limit_rpm: rpmVal ? parseInt(rpmVal, 10) : null,
        budget_micro_usd_month: budgetVal ? Math.round(parseFloat(budgetVal) * 1000000) : null
    };

    try {
        const res = await fetch('/api/user/client-keys', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify(payload)
        });
        const data = await res.json();
        closeModal('modal-new-key');
        document.getElementById('form-new-key').reset();

        // Refresh HTMX client keys table
        const tbody = document.getElementById('client-keys-body');
        if (tbody) htmx.trigger(tbody, 'load');

        if (data.raw_token) {
            document.getElementById('revealed-token-text').textContent = data.raw_token;
            openModal('modal-token-reveal');
        }
    } catch (err) {
        alert('Failed to generate key: ' + err);
    }
}

async function handleCreateProvider(e) {
    e.preventDefault();
    const name = document.getElementById('prov-name').value.trim();
    const kind = document.getElementById('prov-kind').value;
    const base_url = document.getElementById('prov-url').value.trim();
    const allow_private_network = document.getElementById('prov-private-net').checked;

    try {
        await fetch('/api/user/providers', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ name, kind, base_url, config_json: '{}', allow_private_network })
        });
        closeModal('modal-new-provider');
        document.getElementById('form-new-provider').reset();

        const list = document.getElementById('providers-list');
        if (list) htmx.trigger(list, 'load');
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

    try {
        await fetch(`/api/user/providers/${provider_id}/keys`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify(payload)
        });
        closeModal('modal-new-upstream-key');
        document.getElementById('form-new-upstream-key').reset();

        const list = document.getElementById('providers-list');
        if (list) htmx.trigger(list, 'load');
        const gList = document.getElementById('adm-providers-list');
        if (gList) htmx.trigger(gList, 'load');
    } catch (err) {
        alert('Failed to add upstream key: ' + err);
    }
}

async function handleCreateShare(e) {
    e.preventDefault();
    const grantee_user_id = document.getElementById('share-grantee').value.trim();
    const provider_id = document.getElementById('share-provider-select').value;
    const capVal = document.getElementById('share-cap').value;
    const modelsVal = document.getElementById('share-models').value.trim();

    const payload = {
        grantee_user_id,
        provider_id,
        upstream_key_id: null,
        max_monthly_micro_usd: capVal ? Math.round(parseFloat(capVal) * 1000000) : null,
        allowed_models: modelsVal ? modelsVal.split(',').map(s => s.trim()) : null
    };

    try {
        await fetch('/api/user/shares', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify(payload)
        });
        closeModal('modal-new-share');
        document.getElementById('form-new-share').reset();

        const outBody = document.getElementById('outgoing-shares-body');
        if (outBody) htmx.trigger(outBody, 'load');
    } catch (err) {
        alert('Failed to grant share: ' + err);
    }
}

async function handleCreateUser(e) {
    e.preventDefault();
    const username = document.getElementById('usr-username').value.trim();
    const password = document.getElementById('usr-password').value;
    const role = document.getElementById('usr-role').value;

    try {
        await fetch('/admin/api/users', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ username, password, role })
        });
        closeModal('modal-new-user');
        document.getElementById('form-new-user').reset();

        const uBody = document.getElementById('adm-users-body');
        if (uBody) htmx.trigger(uBody, 'load');
    } catch (err) {
        alert('Failed to create user: ' + err);
    }
}

async function handleCreateGlobalProvider(e) {
    e.preventDefault();
    const name = document.getElementById('gp-name').value.trim();
    const kind = document.getElementById('gp-kind').value;
    const base_url = document.getElementById('gp-url').value.trim();
    const allow_private_network = document.getElementById('gp-private-net').checked;

    try {
        await fetch('/admin/api/providers', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ name, kind, base_url, config_json: '{}', allow_private_network })
        });
        closeModal('modal-new-global-prov');
        document.getElementById('form-new-global-prov').reset();

        const gList = document.getElementById('adm-providers-list');
        if (gList) htmx.trigger(gList, 'load');
    } catch (err) {
        alert('Failed to create global provider: ' + err);
    }
}

async function handleAddAlias(e) {
    e.preventDefault();
    const alias = document.getElementById('alias-name').value.trim();
    const target = document.getElementById('alias-target-select').value;

    try {
        await fetch(`/admin/api/models/${target}/aliases`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ alias })
        });
        closeModal('modal-new-alias');
        document.getElementById('form-new-alias').reset();

        const mBody = document.getElementById('adm-models-body');
        if (mBody) htmx.trigger(mBody, 'load');
    } catch (err) {
        alert('Failed to add alias: ' + err);
    }
}

async function testUpstreamKey(keyId) {
    try {
        const res = await fetch(`/api/user/upstream-keys/${keyId}/test`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: '{}'
        });
        const data = await res.json();
        alert(data.message || 'Key test completed!');
    } catch (e) {
        alert('Key test failed: ' + e);
    }
}

async function toggleUser(id, disabled) {
    try {
        await fetch(`/admin/api/users/${id}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ disabled })
        });
        const uBody = document.getElementById('adm-users-body');
        if (uBody) htmx.trigger(uBody, 'load');
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function toggleModelExposed(modelPk, exposed) {
    try {
        await fetch(`/admin/api/models/${modelPk}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ exposed })
        });
        const mBody = document.getElementById('adm-models-body');
        if (mBody) htmx.trigger(mBody, 'load');
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function saveSetting(key) {
    const input = document.getElementById(`setting-${key}`);
    if (!input) return;
    let value = input.value;
    if (input.type === 'number') value = parseInt(value, 10);
    else if (input.type === 'checkbox') value = input.checked;

    try {
        await fetch('/admin/api/settings', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ [key]: value })
        });
        alert(`Setting "${key}" updated!`);
    } catch (e) {
        alert('Failed to update setting: ' + e);
    }
}

async function openChatPreview(sessionHash) {
    const modal = document.getElementById('modal-chat-preview');
    const container = document.getElementById('chat-preview-turns');
    const hashSpan = document.getElementById('chat-preview-hash');
    if (!modal || !container) return;

    if (hashSpan) hashSpan.textContent = sessionHash.slice(0, 16) + '...';
    container.innerHTML = '<p class="text-muted">Reconstructing multi-turn session timeline...</p>';
    openModal('modal-chat-preview');

    try {
        const res = await fetch(`/api/user/sessions/${sessionHash}/chat`);
        if (!res.ok) {
            container.innerHTML = '<p class="text-muted">No stored trace turns found for this session (traceability may be disabled or retention expired).</p>';
            return;
        }
        const data = await res.json();
        if (!data.timeline || data.timeline.length === 0) {
            container.innerHTML = '<p class="text-muted">Zero recorded turns for this session.</p>';
            return;
        }

        container.innerHTML = data.timeline.map(turn => {
            const inputHtml = (turn.new_input_messages || []).map(m => `
                <div class="chat-msg user-msg">
                    <div class="chat-role">User</div>
                    <pre class="chat-content">${m.content ? m.content : JSON.stringify(m)}</pre>
                </div>
            `).join('');

            const reasoningHtml = turn.assistant_reasoning ? `
                <details class="chat-reasoning">
                    <summary>Thinking / Reasoning</summary>
                    <pre>${turn.assistant_reasoning}</pre>
                </details>
            ` : '';

            const toolCallsHtml = (turn.assistant_tool_calls || []).map(tc => `
                <div class="chat-tool-call">
                    <span class="badge badge-warning">Tool Call: ${tc.name || tc.id}</span>
                    <pre>${JSON.stringify(tc.arguments || tc.parameters || tc, null, 2)}</pre>
                </div>
            `).join('');

            return `
                <div class="chat-turn-card">
                    <div class="chat-turn-header">
                        <span>Turn #${turn.ordinal} (${turn.canonical_model})</span>
                        <span class="text-muted">${new Date(turn.ts).toLocaleTimeString()}</span>
                    </div>
                    ${inputHtml}
                    ${reasoningHtml}
                    ${toolCallsHtml}
                    <div class="chat-msg assistant-msg">
                        <div class="chat-role">Assistant</div>
                        <div class="chat-assistant-body">${turn.assistant_text || '<em>(no text output)</em>'}</div>
                    </div>
                </div>
            `;
        }).join('');
    } catch (e) {
        container.innerHTML = `<p class="text-danger">Failed to load chat preview: ${e}</p>`;
    }
}
