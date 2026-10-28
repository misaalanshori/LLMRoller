// LLMRoller Web UI — settings
// Runtime settings grid and user-preference forms.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

const ROUTING_STRATEGIES = [
    'smooth_weighted_rr',
    'naive_rr',
    'weighted_rr',
    'uniform_random',
    'weighted_random',
    'power_of_two_choices'
];

async function loadAdminSettings() {
    const grid = document.getElementById('adm-settings-grid');
    if (!grid) return;
    try {
        const res = await fetch('/admin/api/settings');
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const settings = await res.json();
        const tc = settings.traceability_config;
        if (tc && typeof tc === 'object') {
            const trcEnabled = document.getElementById('trc-enabled');
            const trcDays = document.getElementById('trc-retention-days');
            if (trcEnabled) trcEnabled.checked = Boolean(tc.enabled);
            if (trcDays && tc.retention_days != null) trcDays.value = String(tc.retention_days);
        }
        const keys = Object.keys(settings).sort();
        grid.innerHTML = keys.map(key => {
            const id = `setting-${key}`;
            const value = settings[key];
            const label = key.replace(/_/g, ' ');
            let input;
            if (key === 'traceability_config') {
                input = `<textarea id="${id}" rows="5" disabled>${escapeHtml(JSON.stringify(value, null, 2))}</textarea>`;
            } else if (key === 'default_routing_strategy') {
                const opts = ROUTING_STRATEGIES.map(s =>
                    `<option value="${s}" ${value === s ? 'selected' : ''}>${s}</option>`
                ).join('');
                input = `<select id="${id}">${opts}</select>`;
            } else if (typeof value === 'boolean') {
                input = `<label class="checkbox-group" for="${id}"><input type="checkbox" id="${id}" ${value ? 'checked' : ''}><span>${value ? 'Enabled' : 'Disabled'}</span></label>`;
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
                <div>
                    <div class="setting-key">${escapeHtml(key)}</div>
                    <div class="setting-desc">${escapeHtml(label)}</div>
                </div>
                <div class="setting-control">${input}</div>
                <div class="setting-footer">${save}</div>
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

async function handleSaveTraceabilityConfig(e) {
    e.preventDefault();
    const enabled = document.getElementById('trc-enabled').checked;
    const retention_days = parseInt(document.getElementById('trc-retention-days').value || '14', 10);
    try {
        let res = await fetch('/admin/api/settings/traceability', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ enabled, retention_days })
        });
        if (res.status === 409) {
            const body = await res.json().catch(() => ({}));
            const affected = body.error?.affected_rows ?? 0;
            if (!confirm(`Lowering retention to ${retention_days} days will purge ${affected} older trace record(s). Confirm?`)) {
                return;
            }
            res = await fetch('/admin/api/settings/traceability', {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
                body: JSON.stringify({ enabled, retention_days, confirm_purge: true })
            });
        }
        if (!res.ok) throw new Error(await extractApiError(res));
        await loadAdminSettings();
        await refreshPartial('adm-audit-body');
        alert('Traceability capture policy saved.');
    } catch (err) {
        alert('Failed to update traceability config: ' + err);
    }
}

function syncUserPreferenceInputs() {
    const htmlEl = document.documentElement;
    const themeSel = document.getElementById('pref-theme');
    const navSel = document.getElementById('pref-nav-layout');
    const locSel = document.getElementById('pref-locale');
    if (themeSel && htmlEl.dataset.theme) themeSel.value = htmlEl.dataset.theme;
    if (navSel && htmlEl.dataset.navLayout) navSel.value = htmlEl.dataset.navLayout;
    if (locSel && htmlEl.lang) locSel.value = htmlEl.lang;
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
        if (!res.ok) throw new Error(await extractApiError(res));
        document.documentElement.dataset.theme = ui_theme;
        document.documentElement.dataset.navLayout = ui_nav_layout;
        document.documentElement.lang = ui_locale;
        window.location.reload();
    } catch (err) {
        alert('Failed to save preferences: ' + err);
    }
}
