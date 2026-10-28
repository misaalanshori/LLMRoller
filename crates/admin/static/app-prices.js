// LLMRoller Web UI — prices
// Price book entries.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

async function loadAdminPrices() {
    const tbody = document.getElementById('adm-prices-body');
    if (!tbody) return;
    try {
        const res = await fetch('/admin/api/prices');
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const prices = await res.json();
        if (!Array.isArray(prices) || prices.length === 0) {
            tbody.innerHTML = '<tr><td colspan="7" class="text-center text-muted">No price entries configured.</td></tr>';
            return;
        }
        const fmtRate = v => `$${(Number(v || 0) / 1_000_000).toFixed(4)}`;
        tbody.innerHTML = prices.map(p => {
            const pid = p.provider_id || '';
            const mid = p.upstream_model_id || '';
            const src = p.source || 'admin';
            return `<tr>
                <td><strong>${escapeHtml(mid)}</strong></td>
                <td>${fmtRate(p.input_per_mtok_micro_usd)}</td>
                <td>${fmtRate(p.output_per_mtok_micro_usd)}</td>
                <td>${fmtRate(p.cache_read_per_mtok_micro_usd)}</td>
                <td>${fmtRate(p.cache_write_per_mtok_micro_usd)}</td>
                <td><span class="badge badge-info">${escapeHtml(src)}</span></td>
                <td><button class="btn btn-xs btn-danger" data-action="delete-price" data-provider-id="${escapeHtml(pid)}" data-model-id="${escapeHtml(mid)}">Delete</button></td>
            </tr>`;
        }).join('');
    } catch (err) {
        tbody.innerHTML = `<tr><td colspan="7" class="text-center text-danger">Failed to load prices: ${escapeHtml(err)}</td></tr>`;
    }
}

async function handleSavePriceEntry(e) {
    e.preventDefault();
    const upstream_model_id = document.getElementById('prc-model-id').value.trim();
    const input_per_mtok_micro_usd = Math.round(parseFloat(document.getElementById('prc-input').value || '0') * 1_000_000);
    const output_per_mtok_micro_usd = Math.round(parseFloat(document.getElementById('prc-output').value || '0') * 1_000_000);
    try {
        const res = await fetch('/admin/api/prices', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({
                provider_id: '',
                provider_kind: 'custom',
                upstream_model_id,
                input_per_mtok_micro_usd,
                output_per_mtok_micro_usd,
                cache_read_per_mtok_micro_usd: Math.round(input_per_mtok_micro_usd / 10),
                cache_write_per_mtok_micro_usd: input_per_mtok_micro_usd
            })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-price');
        document.getElementById('form-new-price').reset();
        await invalidateAndRefetch('prices');
    } catch (err) {
        alert('Failed to save price entry: ' + err);
    }
}

async function deletePriceEntry(providerId, modelId) {
    if (!modelId || !confirm(`Delete price entry for "${modelId}"?`)) return;
    try {
        const url = `/admin/api/prices?provider_id=${encodeURIComponent(providerId)}&upstream_model_id=${encodeURIComponent(modelId)}`;
        const res = await fetch(url, {
            method: 'DELETE',
            headers: { 'X-CSRF-Token': getCsrfToken() }
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('prices');
    } catch (err) {
        alert('Failed to delete price entry: ' + err);
    }
}
