// LLMRoller Web UI — accounts
// Service accounts and their budget windows.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

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
            <td><button class="btn btn-xs btn-danger" data-action="delete-service-account" data-id="${escapeHtml(sa.id || '')}">Delete</button></td>
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
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-service-account');
        document.getElementById('form-new-service-account').reset();
        await loadServiceAccounts();
        await refreshPartial('user-audit-body');
    } catch (err) {
        alert('Failed to create service account: ' + err);
    }
}

async function deleteServiceAccount(id) {
    if (!id || !confirm('Delete this service account and revoke its child keys?')) return;
    try {
        const res = await fetch(`/api/user/service-accounts/${encodeURIComponent(id)}`, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await loadServiceAccounts();
        await invalidateAndRefetch('client-keys');
    } catch (err) {
        alert('Failed to delete service account: ' + err);
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
    const apiBase = window.location.pathname.startsWith('/admin') ? '/admin/api' : '/api/user';
    try {
        const res = await fetch(`${apiBase}/upstream-keys/${encodeURIComponent(upstream_key_id)}/windows`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ scope_kind: 'upstream_key', window_kind, period_ms, limit_micro_usd })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-budget-window');
        await invalidateAndRefetch('keys');
    } catch (err) {
        alert('Failed to save budget window: ' + err);
    }
}
