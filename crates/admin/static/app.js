// LLMRoller Web UI Vanilla JS Helpers & HTMX Configuration

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
    navigator.clipboard.writeText(el.innerText || el.textContent);
    const orig = el.textContent;
    alert("Copied to clipboard!");
}

function onProviderKindChange(kind) {
    const urlInput = document.getElementById('prov-url');
    if (!urlInput) return;
    if (kind === 'openrouter') urlInput.value = 'https://openrouter.ai/api/v1';
    else if (kind === 'opencode_go') urlInput.value = 'https://opencode.ai/zen/go/v1';
    else if (kind === 'command_code') urlInput.value = 'https://commandcode.ai/alpha';
    else if (kind === 'custom') urlInput.value = '';
}

// ---------------------------------------------------------------------------
// Personal Workspace Data Loaders
// ---------------------------------------------------------------------------

async function loadClientKeys() {
    const tbody = document.getElementById('client-keys-body');
    if (!tbody) return;
    try {
        const res = await fetch('/api/user/client-keys');
        const keys = await res.json();
        const stat = document.getElementById('stat-keys-count');
        if (stat) stat.textContent = keys.length;

        if (keys.length === 0) {
            tbody.innerHTML = '<tr><td colspan="7" class="text-center text-muted">No client keys issued yet. Click "+ Generate Key" above.</td></tr>';
            return;
        }

        tbody.innerHTML = keys.map(k => `
            <tr>
                <td><code>${k.prefix}...</code></td>
                <td><strong>${k.label}</strong></td>
                <td><span class="badge badge-info">${k.routing_scope}</span></td>
                <td>${k.rate_limit_rpm ? k.rate_limit_rpm + ' RPM' : 'Unlimited'}</td>
                <td>${k.budget_micro_usd_month ? '$' + (k.budget_micro_usd_month / 1000000).toFixed(2) : 'No Cap'}</td>
                <td>${new Date(k.created_at).toLocaleDateString()}</td>
                <td>
                    <button class="btn btn-xs btn-danger" onclick="deleteClientKey('${k.id}')">Revoke</button>
                </td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = `<tr><td colspan="7" class="text-center text-muted">Error loading client keys: ${e}</td></tr>`;
    }
}

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
        loadClientKeys();

        if (data.raw_token) {
            document.getElementById('revealed-token-text').textContent = data.raw_token;
            openModal('modal-token-reveal');
        }
    } catch (err) {
        alert('Failed to create key: ' + err);
    }
}

async function deleteClientKey(id) {
    if (!confirm('Are you sure you want to revoke this client key? Any agent using it will be locked out immediately.')) return;
    try {
        await fetch(`/api/user/client-keys/${id}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadClientKeys();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function loadProviders() {
    const list = document.getElementById('providers-list');
    if (!list) return;
    try {
        const res = await fetch('/api/user/providers');
        const provs = await res.json();
        const stat = document.getElementById('stat-providers-count');
        if (stat) stat.textContent = provs.length;

        // Also update share select
        const shareSelect = document.getElementById('share-provider-select');
        if (shareSelect) {
            shareSelect.innerHTML = provs.map(p => `<option value="${p.id}">${p.name} (${p.kind})</option>`).join('');
        }

        if (provs.length === 0) {
            list.innerHTML = '<p class="text-muted text-center" style="padding: 2rem 0;">No personal providers configured yet. Click "+ Add Provider" above.</p>';
            return;
        }

        list.innerHTML = provs.map(p => `
            <div class="provider-box">
                <div class="provider-header">
                    <div>
                        <strong>${p.name}</strong>
                        <span class="badge badge-info" style="margin-left: 0.5rem;">${p.kind}</span>
                        ${p.allow_private_network ? '<span class="badge badge-warning">Private Net</span>' : ''}
                        <div class="text-muted" style="font-size: 0.8125rem; font-family: monospace;">${p.base_url}</div>
                    </div>
                    <div>
                        <button class="btn btn-xs btn-primary" onclick="openAddKeyModal('${p.id}', '${p.name}')">+ Add Key</button>
                        <button class="btn btn-xs btn-danger" onclick="deleteProvider('${p.id}')">Delete</button>
                    </div>
                </div>
                <div id="keys-for-prov-${p.id}" class="keys-sublist">
                    <button class="btn btn-xs btn-ghost" onclick="loadKeysForProvider('${p.id}')">Load Upstream Keys...</button>
                </div>
            </div>
        `).join('');

        // Automatically load keys for all providers
        provs.forEach(p => loadKeysForProvider(p.id));
    } catch (e) {
        list.innerHTML = `<p class="text-muted">Error loading providers: ${e}</p>`;
    }
}

async function loadKeysForProvider(provId) {
    const el = document.getElementById(`keys-for-prov-${provId}`);
    if (!el) return;
    try {
        const res = await fetch(`/api/user/providers/${provId}/keys`);
        const keys = await res.json();
        if (keys.length === 0) {
            el.innerHTML = '<p class="text-muted" style="font-size: 0.8125rem; padding: 0.5rem 0;">No upstream keys under this provider. Add one to enable routing.</p>';
            return;
        }
        el.innerHTML = `
            <table class="data-table" style="margin-top: 0.5rem; background: #080c18;">
                <thead><tr><th>Label</th><th>Priority</th><th>Strict Tier</th><th>Health</th><th>Action</th></tr></thead>
                <tbody>
                    ${keys.map(k => `
                        <tr>
                            <td><strong>${k.label}</strong> <span class="text-muted">(${k.secret_last4 ? '••••' + k.secret_last4 : 'sealed'})</span></td>
                            <td>${k.priority}</td>
                            <td>Tier ${k.strict_tier}</td>
                            <td><span class="badge badge-${k.enabled ? 'success' : 'danger'}">${k.enabled ? 'Active' : 'Disabled'}</span></td>
                            <td>
                                <button class="btn btn-xs btn-ghost" onclick="testUpstreamKey('${k.id}')">Test</button>
                                <button class="btn btn-xs btn-danger" onclick="deleteUpstreamKey('${k.id}', '${provId}')">Remove</button>
                            </td>
                        </tr>
                    `).join('')}
                </tbody>
            </table>
        `;
    } catch (e) {
        el.innerHTML = `<p class="text-muted">Error: ${e}</p>`;
    }
}

function openAddKeyModal(provId, provName) {
    document.getElementById('target-provider-id').value = provId;
    openModal('modal-new-upstream-key');
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
        loadProviders();
    } catch (err) {
        alert('Failed to add provider: ' + err);
    }
}

async function deleteProvider(id) {
    if (!confirm('Delete this provider and all attached upstream keys?')) return;
    try {
        await fetch(`/api/user/providers/${id}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadProviders();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function handleCreateUpstreamKey(e) {
    e.preventDefault();
    const provId = document.getElementById('target-provider-id').value;
    const label = document.getElementById('uk-label').value.trim();
    const secret = document.getElementById('uk-secret').value.trim();
    const priority = parseInt(document.getElementById('uk-priority').value, 10);
    const strict_tier = parseInt(document.getElementById('uk-tier').value, 10);

    try {
        await fetch(`/api/user/providers/${provId}/keys`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ label, secret, priority, strict_tier, enabled: true })
        });
        closeModal('modal-new-upstream-key');
        document.getElementById('form-new-upstream-key').reset();
        loadKeysForProvider(provId);
    } catch (err) {
        alert('Failed to add key: ' + err);
    }
}

async function deleteUpstreamKey(id, provId) {
    if (!confirm('Remove this upstream key?')) return;
    try {
        await fetch(`/api/user/upstream-keys/${id}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadKeysForProvider(provId);
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function loadShares() {
    const outBody = document.getElementById('outgoing-shares-body');
    const inBody = document.getElementById('incoming-shares-body');
    if (!outBody || !inBody) return;

    try {
        const res = await fetch('/api/user/shares');
        const data = await res.json();

        if (data.outgoing.length === 0) {
            outBody.innerHTML = '<tr><td colspan="5" class="text-center text-muted">No outgoing shares created.</td></tr>';
        } else {
            outBody.innerHTML = data.outgoing.map(s => `
                <tr>
                    <td><code>${s.grantee_user_id.slice(0, 10)}...</code></td>
                    <td>${s.upstream_key_id ? 'Key-Level' : 'Provider-Level'}</td>
                    <td>${s.max_monthly_micro_usd ? '$' + (s.max_monthly_micro_usd / 1000000).toFixed(2) : 'No Cap'}</td>
                    <td><span class="badge badge-${s.enabled ? 'success' : 'danger'}">${s.enabled ? 'Active' : 'Suspended/Revoked'}</span></td>
                    <td><button class="btn btn-xs btn-danger" onclick="deleteShare('${s.id}')">Revoke</button></td>
                </tr>
            `).join('');
        }

        if (data.incoming.length === 0) {
            inBody.innerHTML = '<tr><td colspan="5" class="text-center text-muted">No shares shared with you.</td></tr>';
        } else {
            inBody.innerHTML = data.incoming.map(s => {
                const isMatching = s.accepted_config_fingerprint === s.current_fingerprint;
                return `
                    <tr>
                        <td><strong>${s.provider_name}</strong> <span class="badge badge-info">${s.provider_kind}</span></td>
                        <td><code>${s.owner_user_id.slice(0, 10)}...</code></td>
                        <td>${s.allowed_models ? s.allowed_models : 'All'}</td>
                        <td><span class="badge badge-${isMatching ? 'success' : 'warning'}">${isMatching ? 'Accepted' : 'Pending Consent'}</span></td>
                        <td>
                            ${!isMatching ? `<button class="btn btn-xs btn-primary" onclick="acceptShare('${s.id}', '${s.current_fingerprint}')">Accept Share</button>` : '<span class="text-muted">Active</span>'}
                        </td>
                    </tr>
                `;
            }).join('');
        }
    } catch (e) {
        console.error(e);
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
        loadShares();
    } catch (err) {
        alert('Failed to grant share: ' + err);
    }
}

async function acceptShare(id, fingerprint) {
    try {
        await fetch(`/api/user/shares/${id}/accept`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ fingerprint })
        });
        loadShares();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function deleteShare(id) {
    if (!confirm('Revoke this share grant? Grantee preferences will be removed immediately.')) return;
    try {
        await fetch(`/api/user/shares/${id}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadShares();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function loadActivePins() {
    const tbody = document.getElementById('active-pins-body');
    if (!tbody) return;
    try {
        const res = await fetch('/api/user/pins');
        const pins = await res.json();
        const stat = document.getElementById('stat-sessions-count');
        if (stat) stat.textContent = pins.length;

        if (pins.length === 0) {
            tbody.innerHTML = '<tr><td colspan="7" class="text-center text-muted">No active sessions pinned right now.</td></tr>';
            return;
        }

        tbody.innerHTML = pins.map(p => `
            <tr>
                <td><code>${p.session_hash.slice(0, 12)}...</code></td>
                <td><code>${p.client_key_id.slice(0, 10)}...</code></td>
                <td><strong>${p.canonical_id || 'unresolved'}</strong></td>
                <td>${p.key_label || p.upstream_key_id.slice(0, 8)}</td>
                <td><span class="badge badge-info">Tier ${p.pool_tier}</span></td>
                <td>${new Date(p.last_used_at).toLocaleTimeString()}</td>
                <td><button class="btn btn-xs btn-danger" onclick="deletePin('${p.session_hash}')">Unpin</button></td>
            </tr>
        `).join('');
    } catch (e) {
        console.error(e);
    }
}

async function deletePin(hash) {
    try {
        await fetch(`/admin/api/sessions/${hash}/pin`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadActivePins();
    } catch (e) {
        alert('Error: ' + e);
    }
}

// ---------------------------------------------------------------------------
// Admin Console Data Loaders
// ---------------------------------------------------------------------------

async function loadAdminUsers() {
    const tbody = document.getElementById('adm-users-body');
    if (!tbody) return;
    try {
        const res = await fetch('/admin/api/users');
        const users = await res.json();
        const stat = document.getElementById('stat-adm-users-count');
        if (stat) stat.textContent = users.length;

        tbody.innerHTML = users.map(u => `
            <tr>
                <td><strong>${u.username}</strong></td>
                <td><span class="badge badge-${u.role === 'admin' ? 'warning' : 'info'}">${u.role}</span></td>
                <td>${u.can_use_global_providers ? '✅ Enabled' : '❌ Restricted'}</td>
                <td>${u.global_budget_micro_usd_month ? '$' + (u.global_budget_micro_usd_month / 1000000).toFixed(2) : 'No Cap'}</td>
                <td><span class="badge badge-${u.disabled ? 'danger' : 'success'}">${u.disabled ? 'Disabled' : 'Active'}</span></td>
                <td>${new Date(u.created_at).toLocaleDateString()}</td>
                <td>
                    <button class="btn btn-xs btn-secondary" onclick="toggleUser('${u.id}', ${!u.disabled})">${u.disabled ? 'Enable' : 'Disable'}</button>
                    ${u.role !== 'admin' ? `<button class="btn btn-xs btn-danger" onclick="deleteUser('${u.id}', '${u.username}')">Delete</button>` : ''}
                </td>
            </tr>
        `).join('');
    } catch (e) {
        console.error(e);
    }
}

async function handleCreateUser(e) {
    e.preventDefault();
    const username = document.getElementById('u-username').value.trim();
    const password = document.getElementById('u-password').value;
    const role = document.getElementById('u-role').value;

    try {
        await fetch('/admin/api/users', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ username, password, role })
        });
        closeModal('modal-new-user');
        document.getElementById('form-new-user').reset();
        loadAdminUsers();
    } catch (err) {
        alert('Failed to create user: ' + err);
    }
}

async function toggleUser(id, disabled) {
    try {
        await fetch(`/admin/api/users/${id}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ disabled })
        });
        loadAdminUsers();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function deleteUser(id, username) {
    const confirmName = prompt(`Type "${username}" to confirm deletion:`);
    if (confirmName !== username) return;

    try {
        await fetch(`/admin/api/users/${id}?confirm=${encodeURIComponent(confirmName)}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        loadAdminUsers();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function loadAdminProviders() {
    const list = document.getElementById('adm-providers-list');
    if (!list) return;
    try {
        const res = await fetch('/admin/api/providers');
        const provs = await res.json();
        const stat = document.getElementById('stat-adm-prov-count');
        if (stat) stat.textContent = provs.length;

        if (provs.length === 0) {
            list.innerHTML = '<p class="text-muted text-center" style="padding: 2rem 0;">No global providers configured. Click "+ Add Global Provider" above.</p>';
            return;
        }

        list.innerHTML = provs.map(p => `
            <div class="provider-box">
                <div class="provider-header">
                    <div>
                        <strong>${p.name}</strong>
                        <span class="badge badge-warning" style="margin-left: 0.5rem;">Global</span>
                        <span class="badge badge-info">${p.kind}</span>
                        <div class="text-muted" style="font-size: 0.8125rem; font-family: monospace;">${p.base_url}</div>
                    </div>
                    <div>
                        <button class="btn btn-xs btn-primary" onclick="openAddKeyModal('${p.id}', '${p.name}')">+ Add Global Key</button>
                        <button class="btn btn-xs btn-danger" onclick="deleteProvider('${p.id}')">Delete</button>
                    </div>
                </div>
                <div id="keys-for-prov-${p.id}" class="keys-sublist">
                    <button class="btn btn-xs btn-ghost" onclick="loadKeysForProvider('${p.id}')">Load Keys...</button>
                </div>
            </div>
        `).join('');

        provs.forEach(p => loadKeysForProvider(p.id));
    } catch (e) {
        console.error(e);
    }
}

async function handleCreateGlobalProvider(e) {
    e.preventDefault();
    const name = document.getElementById('gp-name').value.trim();
    const kind = document.getElementById('gp-kind').value;
    const base_url = document.getElementById('gp-url').value.trim();
    const allow_private_network = document.getElementById('gp-private').checked;

    try {
        await fetch('/admin/api/providers', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ name, kind, base_url, config_json: '{}', allow_private_network })
        });
        closeModal('modal-new-global-prov');
        document.getElementById('form-new-global-prov').reset();
        loadAdminProviders();
    } catch (err) {
        alert('Failed: ' + err);
    }
}

async function loadAdminModels() {
    const tbody = document.getElementById('adm-models-body');
    const aliasSelect = document.getElementById('alias-target-pk');
    if (!tbody) return;
    try {
        const res = await fetch('/admin/api/models');
        const data = await res.json();

        if (aliasSelect) {
            aliasSelect.innerHTML = data.models.map(m => `<option value="${m.model_pk}">${m.canonical_id}</option>`).join('');
        }

        if (data.models.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="text-center text-muted">No models discovered or added yet.</td></tr>';
            return;
        }

        tbody.innerHTML = data.models.map(m => {
            const isExposed = m.exposed !== 0;
            return `
                <tr>
                    <td><strong>${m.canonical_id}</strong></td>
                    <td><span class="badge badge-info">${m.origin}</span></td>
                    <td>${m.route_count} Route(s)</td>
                    <td><span class="badge badge-${isExposed ? 'success' : 'danger'}">${isExposed ? 'Exposed' : 'Killswitched'}</span></td>
                    <td>
                        <button class="btn btn-xs btn-${isExposed ? 'danger' : 'success'}" onclick="toggleModelExposed('${m.model_pk}', ${isExposed ? 0 : 1})">
                            ${isExposed ? 'Killswitch' : 'Enable'}
                        </button>
                    </td>
                </tr>
            `;
        }).join('');
    } catch (e) {
        console.error(e);
    }
}

async function toggleModelExposed(pk, exposed) {
    try {
        await fetch(`/admin/api/models/${pk}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ exposed })
        });
        loadAdminModels();
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function handleAddAlias(e) {
    e.preventDefault();
    const alias = document.getElementById('alias-name').value.trim();
    const pk = document.getElementById('alias-target-pk').value;

    try {
        await fetch(`/admin/api/models/${pk}/aliases`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ alias })
        });
        closeModal('modal-new-alias');
        document.getElementById('form-new-alias').reset();
        loadAdminModels();
    } catch (err) {
        alert('Failed: ' + err);
    }
}

async function loadAdminSettings() {
    const container = document.getElementById('adm-settings-container');
    if (!container) return;
    try {
        const res = await fetch('/admin/api/settings');
        const settings = await res.json();

        container.innerHTML = Object.entries(settings).map(([k, v]) => `
            <div class="setting-item">
                <div class="setting-key">${k}</div>
                <div class="form-group" style="margin-bottom: 0.5rem;">
                    <input type="text" value="${v}" id="setting-input-${k}" style="font-size: 0.8125rem;">
                </div>
                <button class="btn btn-xs btn-primary" onclick="saveSetting('${k}')">Save</button>
            </div>
        `).join('');
    } catch (e) {
        console.error(e);
    }
}

async function saveSetting(key) {
    const input = document.getElementById(`setting-input-${key}`);
    if (!input) return;
    const value = input.value.trim();

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

async function loadAuditLog() {
    const tbody = document.getElementById('adm-audit-body');
    if (!tbody) return;
    try {
        const res = await fetch('/admin/api/audit-log');
        const rows = await res.json();

        if (rows.length === 0) {
            tbody.innerHTML = '<tr><td colspan="6" class="text-center text-muted">No audit log entries recorded yet.</td></tr>';
            return;
        }

        tbody.innerHTML = rows.map(r => `
            <tr>
                <td><code>#${r.seq}</code></td>
                <td>${new Date(r.ts).toLocaleString()}</td>
                <td><code>${r.actor_user_id ? r.actor_user_id.slice(0, 10) + '...' : 'System'}</code></td>
                <td><span class="badge badge-info">${r.action}</span></td>
                <td>${r.target_type || '—'} ${r.target_id ? '(' + r.target_id.slice(0, 8) + ')' : ''}</td>
                <td style="font-family: monospace; font-size: 0.75rem; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${r.payload_json}</td>
            </tr>
        `).join('');
    } catch (e) {
        console.error(e);
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

async function loadRoutingMatrix() {
    const tbody = document.getElementById('matrix-body');
    if (!tbody) return;
    try {
        const res = await fetch('/api/user/routes');
        const data = await res.json();
        const models = data.models || [];
        if (models.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="text-center text-muted">No models discovered yet. Add an upstream key or test providers.</td></tr>';
            return;
        }

        tbody.innerHTML = models.map(m => {
            const ownKeysText = m.own_keys.length > 0
                ? m.own_keys.map(k => `${k.label} (Prio: ${k.priority_override})`).join(', ')
                : '—';
            const sharedKeysText = m.shared_keys.length > 0
                ? m.shared_keys.map(k => `${k.label} (Prio: ${k.priority_override})`).join(', ')
                : '—';
            const globalText = m.global_pool.available ? 'Global pool' : '—';

            return `
                <tr>
                    <td><strong>${m.canonical_id}</strong></td>
                    <td><span class="badge badge-info">${m.routing_strategy}</span></td>
                    <td>${ownKeysText}</td>
                    <td>${sharedKeysText}</td>
                    <td><span class="badge badge-success">${globalText}</span></td>
                </tr>
            `;
        }).join('');
    } catch (e) {
        tbody.innerHTML = `<tr><td colspan="5" class="text-center text-muted">Error loading routing matrix: ${e}</td></tr>`;
    }
}

// ---------------------------------------------------------------------------
// Document Ready Dispatcher
// ---------------------------------------------------------------------------
document.addEventListener('DOMContentLoaded', () => {
    // Check if on Workspace
    if (document.getElementById('client-keys-body')) {
        loadClientKeys();
        loadProviders();
        loadShares();
        loadActivePins();
        loadRoutingMatrix();
    }
    // Check if on Admin Console
    if (document.getElementById('adm-users-body')) {
        loadAdminUsers();
        loadAdminProviders();
        loadAdminModels();
        loadAdminSettings();
        loadAuditLog();
    }
});
