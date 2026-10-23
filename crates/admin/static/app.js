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

function escapeHtml(value) {
    const replacements = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
    return String(value ?? '').replace(/[&<>"']/g, ch => replacements[ch]);
}

// Preview fields arrive HTML-escaped from the server (SEC-19). Pass them
// through unchanged, but neutralize any raw tag if a field ever regresses.
function previewText(value) {
    const text = String(value ?? '');
    return /[<>]/.test(text) ? escapeHtml(text) : text;
}

function onProviderKindChange(kind, selectId) {
    const isGlobal = selectId === 'gp-kind';
    const urlInput = document.getElementById(isGlobal ? 'gp-url' : 'prov-url');
    if (!urlInput) return;
    if (kind === 'openrouter') urlInput.value = 'https://openrouter.ai/api/v1';
    else if (kind === 'opencode_go') urlInput.value = 'https://opencode.ai/zen/go/v1';
    else if (kind === 'command_code') urlInput.value = 'https://api.commandcode.ai/v1';
    else if (kind === 'open_design_amr') urlInput.value = 'https://amr.opendesign.ai/v1';
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
        if (modalId === 'modal-new-share') {
            populateShareProviders();
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

    // 9. Open/Close Inspection Drawer (SRDD 14.m)
    const drawerOpen = e.target.closest('[data-action="open-drawer"]');
    if (drawerOpen) {
        const drawer = document.getElementById('right-inspection-drawer');
        if (drawer) drawer.style.display = 'flex';
    }
    const drawerClose = e.target.closest('[data-action="close-drawer"]');
    if (drawerClose) {
        const drawer = document.getElementById('right-inspection-drawer');
        if (drawer) drawer.style.display = 'none';
        return;
    }

    // 9. Save Setting
    const saveSettingBtn = e.target.closest('[data-action="save-setting"]');
    if (saveSettingBtn) {
        const key = saveSettingBtn.getAttribute('data-key');
        saveSetting(key);
        return;
    }

    // 10. Delete Model Alias
    const deleteAliasBtn = e.target.closest('[data-action="delete-alias"]');
    if (deleteAliasBtn) {
        const alias = deleteAliasBtn.getAttribute('data-alias');
        deleteAlias(alias);
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
    if (e.target.id === 'prov-kind' || e.target.id === 'gp-kind' || e.target.id === 'provider-kind-select') {
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

        const revealedToken = data.raw_token || data.token;
        if (revealedToken) {
            document.getElementById('revealed-token-text').textContent = revealedToken;
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
        if (!res.ok) throw new Error('HTTP ' + res.status);
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
        await loadAdminAliases();
    } catch (err) {
        alert('Failed to add alias: ' + err);
    }
}

async function deleteAlias(alias) {
    if (!alias || !confirm(`Delete alias "${alias}"? Clients using it will no longer resolve to its target.`)) {
        return;
    }
    try {
        const res = await fetch(`/admin/api/model-aliases?alias=${encodeURIComponent(alias)}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        if (!res.ok) {
            const body = await res.json().catch(() => ({}));
            throw new Error(body.error?.message || `HTTP ${res.status}`);
        }
        await loadAdminAliases();
        const mBody = document.getElementById('adm-models-body');
        if (mBody) htmx.trigger(mBody, 'load');
    } catch (err) {
        alert('Failed to delete alias: ' + err);
    }
}

async function loadAdminAliases() {
    const tbody = document.getElementById('adm-aliases-body');
    if (!tbody) return;
    try {
        const res = await fetch('/admin/api/models');
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = await res.json();
        const modelNames = new Map((data.models || []).map(m => [m.model_pk, m.canonical_id]));
        const aliases = data.aliases || [];

        const targetSelect = document.getElementById('alias-target-select');
        if (targetSelect) {
            targetSelect.innerHTML = (data.models || []).map(m =>
                `<option value="${escapeHtml(m.model_pk)}">${escapeHtml(m.canonical_id)}</option>`
            ).join('');
        }

        if (aliases.length === 0) {
            tbody.innerHTML = '<tr><td colspan="3" class="text-center text-muted">No confirmed aliases yet.</td></tr>';
            return;
        }
        tbody.innerHTML = aliases.map(alias => {
            const target = modelNames.get(alias.model_pk) || alias.model_pk;
            return `<tr>
                <td><code>${escapeHtml(alias.alias)}</code></td>
                <td><code>${escapeHtml(target)}</code></td>
                <td><button class="btn btn-xs btn-danger" data-action="delete-alias" data-alias="${escapeHtml(alias.alias)}">Delete</button></td>
            </tr>`;
        }).join('');
    } catch (err) {
        tbody.innerHTML = `<tr><td colspan="3" class="text-center text-danger">Failed to load aliases: ${escapeHtml(err)}</td></tr>`;
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

async function loadAdminSettings() {
    const grid = document.getElementById('adm-settings-grid');
    if (!grid) return;
    try {
        const res = await fetch('/admin/api/settings');
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const settings = await res.json();
        const keys = Object.keys(settings).sort();
        grid.innerHTML = keys.map(key => {
            const id = `setting-${key}`;
            const value = settings[key];
            const label = key.replace(/_/g, ' ');
            let input;
            if (key === 'traceability_config') {
                input = `<textarea id="${id}" rows="5" disabled>${escapeHtml(JSON.stringify(value, null, 2))}</textarea>`;
            } else if (typeof value === 'boolean') {
                input = `<input type="checkbox" id="${id}" ${value ? 'checked' : ''}>`;
            } else if (typeof value === 'number') {
                input = `<input type="number" id="${id}" value="${value}">`;
            } else if (typeof value === 'string') {
                input = `<input type="text" id="${id}" value="${escapeHtml(value)}">`;
            } else {
                input = `<textarea id="${id}" data-json="true" rows="4">${escapeHtml(JSON.stringify(value, null, 2))}</textarea>`;
            }
            const save = key === 'traceability_config'
                ? '<span class="text-muted" style="font-size: 0.75rem;">Use the Traceability tab to change this setting.</span>'
                : `<button class="btn btn-xs btn-primary" data-action="save-setting" data-key="${escapeHtml(key)}">Save</button>`;
            return `<div class="setting-item">
                <div class="setting-key">${escapeHtml(key)}</div>
                <div class="setting-desc">${escapeHtml(label)}</div>
                ${input}
                <div style="margin-top: 0.5rem;">${save}</div>
            </div>`;
        }).join('');
    } catch (err) {
        grid.innerHTML = `<p class="text-danger">Failed to load runtime settings: ${escapeHtml(err)}</p>`;
    }
}

async function saveSetting(key) {
    const input = document.getElementById(`setting-${key}`);
    if (!input) return;
    let value;
    if (input.type === 'number') {
        value = Number(input.value);
        if (!Number.isFinite(value)) {
            alert('Please enter a valid number.');
            return;
        }
    } else if (input.type === 'checkbox') {
        value = input.checked;
    } else if (input.dataset.json === 'true') {
        try {
            value = JSON.parse(input.value);
        } catch (e) {
            alert('Invalid JSON: ' + e);
            return;
        }
    } else {
        value = input.value;
    }

    try {
        const res = await fetch('/admin/api/settings', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ [key]: value })
        });
        if (!res.ok) {
            const body = await res.json().catch(() => ({}));
            throw new Error(body.error?.message || `HTTP ${res.status}`);
        }
        await loadAdminSettings();
    } catch (e) {
        alert('Failed to update setting: ' + e);
    }
}

let usageChart = null;

async function loadUsageChart() {
    const container = document.getElementById('uplot-chart');
    if (!container || typeof uPlot === 'undefined') return;
    try {
        const [summaryRes, eventsRes] = await Promise.all([
            fetch('/admin/api/usage/summary'),
            fetch('/admin/api/usage/events?limit=500')
        ]);
        const summary = summaryRes.ok ? await summaryRes.json() : {};
        const eventPayload = eventsRes.ok ? await eventsRes.json() : { events: [] };
        const byDay = new Map();
        for (const event of eventPayload.events || []) {
            const day = Math.floor(Number(event.ts || 0) / 86_400_000) * 86_400_000;
            byDay.set(day, (byDay.get(day) || 0) + Number(event.cost_micro_usd || 0));
        }
        let xs = [...byDay.keys()].sort((a, b) => a - b).map(ms => Math.floor(ms / 1000));
        let ys = xs.map(seconds => (byDay.get(seconds * 1000) || 0) / 1_000_000);
        if (xs.length === 0) {
            xs = [Math.floor(Date.now() / 1000)];
            ys = [Number(summary.total_cost_micro_usd || 0) / 1_000_000];
        }

        const summaryEl = document.getElementById('uplot-summary');
        if (summaryEl) {
            summaryEl.textContent = `${Number(summary.total_requests || 0).toLocaleString()} requests • $${(Number(summary.total_cost_micro_usd || 0) / 1_000_000).toFixed(4)} total spend`;
        }

        if (usageChart) {
            usageChart.destroy();
            usageChart = null;
        }
        container.innerHTML = '';
        container.style.display = 'block';
        usageChart = new uPlot({
            width: container.clientWidth || 640,
            height: 240,
            scales: { x: { time: true } },
            series: [
                {},
                {
                    label: 'USD',
                    stroke: '#38bdf8',
                    width: 2,
                    fill: 'rgba(56, 189, 248, 0.12)'
                }
            ],
            axes: [
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } },
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } }
            ]
        }, [xs, ys], container);
    } catch (err) {
        container.innerHTML = `<span class="text-muted">Usage chart unavailable: ${escapeHtml(err)}</span>`;
    }
}

window.addEventListener('resize', () => {
    const container = document.getElementById('uplot-chart');
    if (usageChart && container) {
        usageChart.setSize({ width: container.clientWidth || 640, height: 240 });
    }
});

document.addEventListener('DOMContentLoaded', () => {
    loadAdminSettings();
    loadAdminAliases();
    loadUsageChart();
});

async function openChatPreview(sessionHash) {
    const modal = document.getElementById('modal-chat-preview');
    const container = document.getElementById('chat-preview-turns');
    const hashSpan = document.getElementById('chat-preview-hash');
    if (!modal || !container) return;

    if (hashSpan) hashSpan.textContent = sessionHash.slice(0, 16) + '...';
    container.innerHTML = '<p class="text-muted">Reconstructing multi-turn session timeline...</p>';
    openModal('modal-chat-preview');

    try {
        const res = await fetch(`/api/user/sessions/${encodeURIComponent(sessionHash)}/chat`);
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
                    <pre class="chat-content">${previewText(m.content ? m.content : JSON.stringify(m))}</pre>
                </div>
            `).join('');

            const reasoningHtml = turn.assistant_reasoning ? `
                <details class="chat-reasoning">
                    <summary>Thinking / Reasoning</summary>
                    <pre>${previewText(turn.assistant_reasoning)}</pre>
                </details>
            ` : '';

            const toolCallsHtml = (turn.assistant_tool_calls || []).map(tc => `
                <div class="chat-tool-call">
                    <span class="badge badge-warning">Tool Call: ${previewText(tc.name || tc.id)}</span>
                    <pre>${previewText(JSON.stringify(tc.arguments || tc.parameters || tc, null, 2))}</pre>
                </div>
            `).join('');

            return `
                <div class="chat-turn-card">
                    <div class="chat-turn-header">
                        <span>Turn #${previewText(turn.ordinal)} (${previewText(turn.canonical_model)})</span>
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
        container.innerHTML = `<p class="text-danger">Failed to load chat preview: ${escapeHtml(e && e.message ? e.message : e)}</p>`;
    }
}
