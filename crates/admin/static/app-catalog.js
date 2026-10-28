// LLMRoller Web UI — catalog
// Canonical model catalog: aliases, exposure toggles, and cross-vendor merge.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

async function handleAddAlias(e) {
    e.preventDefault();
    const alias = document.getElementById('alias-name').value.trim();
    const target = document.getElementById('alias-target-select').value;

    try {
        const res = await fetch(`/admin/api/models/${target}/aliases`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ alias })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-alias');
        document.getElementById('form-new-alias').reset();
        await invalidateAndRefetch('catalog');
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
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('catalog');
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

async function toggleModelExposed(modelPk, exposed) {
    try {
        const res = await fetch(`/admin/api/models/${modelPk}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ exposed })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('catalog');
    } catch (e) {
        alert('Error: ' + e);
    }
}

async function confirmModelMerge(modelPk) {
    if (!modelPk) return;
    try {
        const res = await fetch(`/admin/api/models/${encodeURIComponent(modelPk)}/confirm-merge`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: '{}'
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('catalog');
    } catch (err) {
        alert('Failed to confirm model merge: ' + err);
    }
}
