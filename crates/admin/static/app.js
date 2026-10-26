// LLMRoller Web UI Vanilla JS Event Delegation & HTMX Integration
// All interactions use event delegation on document to strictly adhere to CSP: script-src 'self'

if (typeof htmx !== 'undefined') {
    htmx.config.includeIndicatorStyles = false;
    htmx.config.allowEval = false;
    htmx.config.allowScriptTags = false;
}

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
    else if (kind === 'command_code') urlInput.value = 'https://api.commandcode.ai/provider/v1';
    else if (kind === 'open_design_amr') urlInput.value = 'https://amr-link.open-design.ai/v1';
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
        const keyId = modalOpenBtn.getAttribute('data-key-id');
        if (keyId) {
            const bwKeyInput = document.getElementById('bw-upstream-key-id');
            if (bwKeyInput) bwKeyInput.value = keyId;
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
        if (drawer) drawer.classList.add('open');
    }
    const drawerClose = e.target.closest('[data-action="close-drawer"]');
    if (drawerClose) {
        const drawer = document.getElementById('right-inspection-drawer');
        if (drawer) drawer.classList.remove('open');
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

    // 11. Enroll TOTP 2FA (FND-045)
    const enrollTotpBtn = e.target.closest('[data-action="enroll-totp"]');
    if (enrollTotpBtn) {
        handleEnrollTotp();
        return;
    }

    // 12. Revoke Web Session (FND-045)
    const revokeSessBtn = e.target.closest('[data-action="revoke-web-session"]');
    if (revokeSessBtn) {
        const prefix = revokeSessBtn.getAttribute('data-id-prefix');
        revokeWebSession(prefix);
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
    } else if (e.target.id === 'login-form') {
        handleLoginSubmit(e);
    } else if (e.target.id === 'setup-form') {
        handleSetupSubmit(e);
    } else if (e.target.id === 'form-traceability-config') {
        handleSaveTraceabilityConfig(e);
    } else if (e.target.id === 'form-change-password') {
        handleChangePassword(e);
    } else if (e.target.id === 'form-verify-totp') {
        handleVerifyTotp(e);
    } else if (e.target.id === 'form-user-preferences') {
        handleSaveUserPreferences(e);
    } else if (e.target.id === 'form-new-service-account') {
        handleCreateServiceAccount(e);
    } else if (e.target.id === 'form-budget-window') {
        handleSaveBudgetWindow(e);
    } else if (e.target.id === 'form-new-price') {
        handleSavePriceEntry(e);
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

    const apiBase = window.location.pathname.startsWith('/admin') ? '/admin/api' : '/api/user';
    try {
        await fetch(`${apiBase}/providers/${provider_id}/keys`, {
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
                ? '<span class="text-muted text-xs">Use the Traceability tab to change this setting.</span>'
                : `<button class="btn btn-xs btn-primary" data-action="save-setting" data-key="${escapeHtml(key)}">Save</button>`;
            return `<div class="setting-item">
                <div class="setting-key">${escapeHtml(key)}</div>
                <div class="setting-desc">${escapeHtml(label)}</div>
                ${input}
                <div class="mt-2">${save}</div>
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
        container.classList.remove('hidden');
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
    loadPersonalOverviewAndChart();
    loadServiceAccounts();
    loadWebSessions();
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

async function handleLoginSubmit(e) {
    e.preventDefault();
    const form = e.target;
    const errBox = document.getElementById('error-box');
    const btn = document.getElementById('submit-btn');
    if (errBox) errBox.classList.remove('visible');
    if (btn) {
        btn.disabled = true;
        btn.textContent = 'Signing in...';
    }
    const payload = {
        username: form.username.value.trim(),
        password: form.password.value,
        totp_code: form.totp_code && form.totp_code.value.trim() ? form.totp_code.value.trim() : null
    };
    try {
        const res = await fetch('/api/auth/login', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Accept': 'application/json',
                'X-CSRF-Token': getCsrfToken()
            },
            body: JSON.stringify(payload)
        });
        const data = await res.json().catch(() => ({}));
        if (res.ok) {
            window.location.href = data.role === 'admin' ? '/admin' : '/app';
        } else {
            const msg = (data.error && data.error.message) ? data.error.message : (data.error || ('Authentication failed with status ' + res.status));
            if (errBox) {
                errBox.textContent = msg;
                errBox.classList.add('visible');
            }
            if (btn) {
                btn.disabled = false;
                btn.textContent = 'Sign In';
            }
        }
    } catch (_err) {
        form.submit();
    }
}

async function handleSetupSubmit(e) {
    e.preventDefault();
    const form = e.target;
    const errBox = document.getElementById('error-box');
    const btn = document.getElementById('submit-btn');
    if (errBox) errBox.classList.remove('visible');
    if (btn) {
        btn.disabled = true;
        btn.textContent = 'Initializing...';
    }
    const payload = {
        username: form.username.value.trim(),
        password: form.password.value,
        setup_token: form.setup_token.value.trim()
    };
    try {
        const res = await fetch('/admin/api/setup', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Accept': 'application/json'
            },
            body: JSON.stringify(payload)
        });
        const data = await res.json().catch(() => ({}));
        if (res.ok) {
            window.location.href = '/login?setup_success=1';
        } else {
            const msg = (data.error && data.error.message) ? data.error.message : (data.error || ('Setup failed with status ' + res.status));
            if (errBox) {
                errBox.textContent = msg;
                errBox.classList.add('visible');
            }
            if (btn) {
                btn.disabled = false;
                btn.textContent = 'Initialize Administrator';
            }
        }
    } catch (_err) {
        form.submit();
    }
}

let personalUsageChart = null;

async function loadPersonalOverviewAndChart() {
    const keysEl = document.getElementById('stat-keys-count');
    const provEl = document.getElementById('stat-providers-count');
    const sessEl = document.getElementById('stat-sessions-count');
    if (keysEl || provEl || sessEl) {
        try {
            const [keysRes, provRes, pinsRes] = await Promise.all([
                fetch('/api/user/client-keys'),
                fetch('/api/user/providers'),
                fetch('/api/user/pins')
            ]);
            if (keysEl && keysRes.ok) {
                const keys = await keysRes.json();
                keysEl.textContent = String(Array.isArray(keys) ? keys.length : 0);
            }
            if (provEl && provRes.ok) {
                const provs = await provRes.json();
                provEl.textContent = String(Array.isArray(provs) ? provs.length : 0);
            }
            if (sessEl && pinsRes.ok) {
                const pins = await pinsRes.json();
                sessEl.textContent = String(Array.isArray(pins) ? pins.length : 0);
            }
        } catch (_e) {}
    }

    const container = document.getElementById('personal-usage-chart');
    if (!container || typeof uPlot === 'undefined') return;
    try {
        const [summaryRes, seriesRes] = await Promise.all([
            fetch('/api/user/usage/summary'),
            fetch('/api/user/usage/timeseries')
        ]);
        const summary = summaryRes.ok ? await summaryRes.json() : {};
        const seriesData = seriesRes.ok ? await seriesRes.json() : { series: [] };
        const pts = seriesData.series || [];
        let xs = pts.map(p => Math.floor(Number(p.bucket_ms || 0) / 1000));
        let ys = pts.map(p => Number(p.cost_micro_usd || 0) / 1_000_000);
        if (xs.length === 0) {
            xs = [Math.floor(Date.now() / 1000)];
            ys = [Number(summary.total_cost_micro_usd || 0) / 1_000_000];
        }
        const summaryEl = document.getElementById('personal-uplot-summary');
        if (summaryEl) {
            summaryEl.textContent = `${Number(summary.total_requests || 0).toLocaleString()} requests • $${(Number(summary.total_cost_micro_usd || 0) / 1_000_000).toFixed(4)} personal spend`;
        }
        if (personalUsageChart) {
            personalUsageChart.destroy();
            personalUsageChart = null;
        }
        container.innerHTML = '';
        personalUsageChart = new uPlot({
            width: container.clientWidth || 640,
            height: 240,
            scales: { x: { time: true } },
            series: [{}, { label: 'USD', stroke: '#38bdf8', width: 2, fill: 'rgba(56, 189, 248, 0.12)' }],
            axes: [
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } },
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } }
            ]
        }, [xs, ys], container);
    } catch (err) {
        container.innerHTML = `<span class="text-muted">Personal usage chart unavailable: ${escapeHtml(err)}</span>`;
    }
}

async function handleSaveTraceabilityConfig(e) {
    e.preventDefault();
    const enabled = document.getElementById('trc-enabled').checked;
    const retention_days = parseInt(document.getElementById('trc-retention-days').value || '14', 10);
    try {
        const res = await fetch('/admin/api/settings/traceability', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ enabled, retention_days })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        await loadAdminSettings();
        alert('Traceability capture policy saved.');
    } catch (err) {
        alert('Failed to update traceability config: ' + err);
    }
}

async function handleChangePassword(e) {
    e.preventDefault();
    const current_password = document.getElementById('sec-current-password').value;
    const new_password = document.getElementById('sec-new-password').value;
    try {
        const res = await fetch('/api/user/password', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ current_password, new_password })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        document.getElementById('form-change-password').reset();
        alert('Password updated.');
    } catch (err) {
        alert('Password update failed: ' + err);
    }
}

async function handleEnrollTotp() {
    const box = document.getElementById('totp-enrollment-box');
    if (!box) return;
    try {
        const res = await fetch('/api/user/totp/enroll', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: '{}'
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        const data = await res.json();
        const codes = (data.recovery_codes || []).map(c => `<code>${escapeHtml(c)}</code>`).join(' ');
        box.innerHTML = `<div class="code-block"><pre>${escapeHtml(data.otpauth_url || '')}</pre></div><p class="card-desc mt-2">Recovery Codes: ${codes}</p>`;
    } catch (err) {
        alert('TOTP enrollment failed: ' + err);
    }
}

async function handleVerifyTotp(e) {
    e.preventDefault();
    const code = document.getElementById('sec-totp-code').value.trim();
    try {
        const res = await fetch('/api/user/totp/verify', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ code })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        alert('Two-factor authentication verified and enabled.');
    } catch (err) {
        alert('TOTP verification failed: ' + err);
    }
}

async function loadWebSessions() {
    const tbody = document.getElementById('web-sessions-body');
    if (!tbody) return;
    try {
        const res = await fetch('/api/user/web-sessions');
        if (!res.ok) return;
        const sessions = await res.json();
        if (!Array.isArray(sessions) || sessions.length === 0) {
            tbody.innerHTML = '<tr><td colspan="4" class="text-center text-muted">No active web sessions.</td></tr>';
            return;
        }
        tbody.innerHTML = sessions.map(s => `<tr>
            <td><code>${escapeHtml(s.id_prefix || '')}...</code> ${s.is_current ? '<span class="badge badge-success">Current</span>' : ''}</td>
            <td>${escapeHtml(s.ip_addr || '-')}</td>
            <td>${Number(s.last_seen_at || 0)}</td>
            <td>${s.is_current ? '—' : `<button class="btn btn-xs btn-danger" data-action="revoke-web-session" data-id-prefix="${escapeHtml(s.id_prefix || '')}">Revoke</button>`}</td>
        </tr>`).join('');
    } catch (_e) {}
}

async function revokeWebSession(prefix) {
    if (!prefix) return;
    try {
        await fetch(`/api/user/web-sessions/${encodeURIComponent(prefix)}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        await loadWebSessions();
    } catch (_e) {}
}

async function handleSaveUserPreferences(e) {
    e.preventDefault();
    const ui_theme = document.getElementById('pref-theme').value;
    const ui_nav_layout = document.getElementById('pref-nav-layout').value;
    const ui_locale = document.getElementById('pref-locale').value;
    try {
        const res = await fetch('/api/user/preferences', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ ui_theme, ui_nav_layout, ui_locale })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        window.location.reload();
    } catch (err) {
        alert('Failed to save preferences: ' + err);
    }
}

async function loadServiceAccounts() {
    const tbody = document.getElementById('service-accounts-body');
    if (!tbody) return;
    try {
        const res = await fetch('/api/user/service-accounts');
        if (!res.ok) return;
        const items = await res.json();
        if (!Array.isArray(items) || items.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="text-center text-muted">No service accounts created yet.</td></tr>';
            return;
        }
        tbody.innerHTML = items.map(sa => `<tr>
            <td><strong>${escapeHtml(sa.name || '')}</strong></td>
            <td>${escapeHtml(sa.description || '-')}</td>
            <td><code>${escapeHtml((sa.allowed_cidrs || []).join(', ') || 'Any')}</code></td>
            <td><span class="badge badge-${sa.disabled ? 'danger' : 'success'}">${sa.disabled ? 'Disabled' : 'Active'}</span></td>
            <td><code>${escapeHtml(sa.id || '')}</code></td>
        </tr>`).join('');
    } catch (_e) {}
}

async function handleCreateServiceAccount(e) {
    e.preventDefault();
    const name = document.getElementById('sa-name').value.trim();
    const description = document.getElementById('sa-desc').value.trim();
    const cidrsRaw = document.getElementById('sa-cidrs').value.trim();
    const allowed_cidrs = cidrsRaw ? cidrsRaw.split(',').map(s => s.trim()).filter(Boolean) : [];
    try {
        const res = await fetch('/api/user/service-accounts', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ name, description, allowed_cidrs })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        closeModal('modal-new-service-account');
        document.getElementById('form-new-service-account').reset();
        await loadServiceAccounts();
    } catch (err) {
        alert('Failed to create service account: ' + err);
    }
}

async function handleSaveBudgetWindow(e) {
    e.preventDefault();
    const upstream_key_id = document.getElementById('bw-upstream-key-id').value;
    const window_kind = document.getElementById('bw-window-kind').value;
    const hours = parseInt(document.getElementById('bw-period-hours').value || '5', 10);
    const usd = parseFloat(document.getElementById('bw-limit-usd').value || '10');
    const period_ms = window_kind === 'anchored_calendar_month' ? null : hours * 3600 * 1000;
    const limit_micro_usd = Math.round(usd * 1_000_000);
    try {
        const res = await fetch(`/api/user/upstream-keys/${encodeURIComponent(upstream_key_id)}/budget-windows`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ scope_kind: 'upstream_key', window_kind, period_ms, limit_micro_usd })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        closeModal('modal-budget-window');
    } catch (err) {
        alert('Failed to save budget window: ' + err);
    }
}

async function handleSavePriceEntry(e) {
    e.preventDefault();
    const upstream_model_id = document.getElementById('prc-model-id').value.trim();
    const input_per_mtok_micro_usd = Math.round(parseFloat(document.getElementById('prc-input').value || '0') * 1_000_000);
    const output_per_mtok_micro_usd = Math.round(parseFloat(document.getElementById('prc-output').value || '0') * 1_000_000);
    try {
        const res = await fetch('/admin/api/prices', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({
                provider_id: '',
                upstream_model_id,
                input_per_mtok_micro_usd,
                output_per_mtok_micro_usd,
                cache_read_per_mtok_micro_usd: Math.round(input_per_mtok_micro_usd / 10),
                cache_write_per_mtok_micro_usd: input_per_mtok_micro_usd
            })
        });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        closeModal('modal-new-price');
        document.getElementById('form-new-price').reset();
    } catch (err) {
        alert('Failed to save price entry: ' + err);
    }
}
