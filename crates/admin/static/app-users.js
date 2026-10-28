// LLMRoller Web UI — users
// Tenant accounts.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

async function handleCreateUser(e) {
    e.preventDefault();
    const username = document.getElementById('usr-username').value.trim();
    const password = document.getElementById('usr-password').value;
    const role = document.getElementById('usr-role').value;

    try {
        const res = await fetch('/admin/api/users', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ username, password, role })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        closeModal('modal-new-user');
        document.getElementById('form-new-user').reset();
        await invalidateAndRefetch('users');
    } catch (err) {
        alert('Failed to create user: ' + err);
    }
}

async function toggleUser(id, disabled) {
    try {
        const res = await fetch(`/admin/api/users/${id}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ disabled })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('users');
    } catch (e) {
        alert('Error: ' + e);
    }
}
