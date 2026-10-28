// LLMRoller Web UI — keys
// Client-key lifecycle: create, pause, resume, rotate, reveal, and the
// connection-setup format switcher.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

function copySnippet(id) {
    const el = document.getElementById(id);
    if (!el) return;
    const text = el.innerText || el.textContent;
    navigator.clipboard.writeText(text);
    alert('Copied to clipboard!');
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
        if (!res.ok) throw new Error(await extractApiError(res));
        const data = await res.json();
        closeModal('modal-new-key');
        document.getElementById('form-new-key').reset();
        await invalidateAndRefetch('client-keys');

        const revealedToken = data.raw_token || data.token;
        if (revealedToken) {
            showRevealedTokenModal(revealedToken);
        }
    } catch (err) {
        alert('Failed to generate key: ' + err);
    }
}

let resolvedDataPlaneOrigin = 'http://127.0.0.1:8080';
let activeApiFormat = 'chat';

function selectApiFormat(format, clickedBtn) {
    activeApiFormat = format;
    if (clickedBtn) {
        const grid = clickedBtn.closest('.format-cards-grid');
        if (grid) {
            grid.querySelectorAll('.format-radio-card').forEach(c => c.classList.remove('active'));
        }
        clickedBtn.classList.add('active');
    }
    const badgeEl = document.getElementById('connection-format-badge');
    const urlEl = document.getElementById('connection-base-url');
    if (format === 'messages') {
        if (badgeEl) badgeEl.textContent = 'Anthropic · Messages';
        if (urlEl) urlEl.textContent = resolvedDataPlaneOrigin;
    } else if (format === 'responses') {
        if (badgeEl) badgeEl.textContent = 'OpenAI · Responses';
        if (urlEl) urlEl.textContent = `${resolvedDataPlaneOrigin}/v1`;
    } else {
        if (badgeEl) badgeEl.textContent = 'OpenAI · Chat Completions';
        if (urlEl) urlEl.textContent = `${resolvedDataPlaneOrigin}/v1`;
    }
}

function applyDataPlaneOrigin(origin) {
    resolvedDataPlaneOrigin = origin.replace(/\/+$/, '');
    const ckOpenai = document.getElementById('ck-openai-base-url');
    const ckAnth = document.getElementById('ck-anthropic-base-url');
    const snipClaude = document.getElementById('snippet-claude');
    const snipOpenai = document.getElementById('snippet-openai');
    if (ckOpenai) ckOpenai.textContent = `${resolvedDataPlaneOrigin}/v1`;
    if (ckAnth) ckAnth.textContent = resolvedDataPlaneOrigin;
    if (snipClaude) {
        snipClaude.textContent = `export ANTHROPIC_BASE_URL="${resolvedDataPlaneOrigin}"\nexport ANTHROPIC_API_KEY="llr_live_YOUR_CLIENT_KEY"`;
    }
    if (snipOpenai) {
        snipOpenai.textContent = `export OPENAI_BASE_URL="${resolvedDataPlaneOrigin}/v1"\nexport OPENAI_API_KEY="llr_live_YOUR_CLIENT_KEY"`;
    }
    selectApiFormat(activeApiFormat, null);
}

function showRevealedTokenModal(revealedToken) {
    const tokenEl = document.getElementById('revealed-token-text');
    if (tokenEl) tokenEl.textContent = revealedToken;
    const snipEl = document.getElementById('revealed-token-snippet');
    if (snipEl) {
        snipEl.textContent =
            `export OPENAI_BASE_URL="${resolvedDataPlaneOrigin}/v1"\n` +
            `export OPENAI_API_KEY="${revealedToken}"\n` +
            `export ANTHROPIC_BASE_URL="${resolvedDataPlaneOrigin}"\n` +
            `export ANTHROPIC_API_KEY="${revealedToken}"`;
    }
    openModal('modal-token-reveal');
}

async function initConnectionSetupUrls() {
    const proto = window.location.protocol || 'http:';
    const host = window.location.hostname || '127.0.0.1';
    const port = Number(window.location.port || 0);
    const candidates = [];
    if (port === 8081) {
        candidates.push(`${proto}//${host}:${port + 1}`, `${proto}//${host}:8080`);
    } else if (port > 0) {
        candidates.push(`${proto}//${host}:${port}`, `${proto}//${host}:8080`);
    } else {
        candidates.push(`${proto}//${host}`);
    }
    for (const candidate of candidates) {
        try {
            const ctrl = new AbortController();
            const timer = setTimeout(() => ctrl.abort(), 600);
            const res = await fetch(`${candidate}/healthz`, { mode: 'no-cors', signal: ctrl.signal });
            clearTimeout(timer);
            if (res) {
                applyDataPlaneOrigin(candidate);
                return;
            }
        } catch (_e) {}
    }
    applyDataPlaneOrigin(candidates[0] || 'http://127.0.0.1:8080');
}

async function toggleClientKeyPause(id, pause) {
    if (!id) return;
    const action = pause ? 'pause' : 'resume';
    try {
        const res = await fetch(`/api/user/client-keys/${encodeURIComponent(id)}/${action}`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: '{}'
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        await invalidateAndRefetch('client-keys');
    } catch (err) {
        alert(`Failed to ${action} client key: ` + err);
    }
}

async function rotateClientKey(id) {
    if (!id || !confirm('Rotate this client key? The previous token will remain valid for the 5-minute grace window.')) return;
    try {
        const res = await fetch(`/api/user/client-keys/${encodeURIComponent(id)}/rotate`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': getCsrfToken() },
            body: JSON.stringify({ grace_period_secs: 300 })
        });
        if (!res.ok) throw new Error(await extractApiError(res));
        const data = await res.json();
        await invalidateAndRefetch('client-keys');
        const revealedToken = data.raw_token || data.token;
        if (revealedToken) {
            showRevealedTokenModal(revealedToken);
        }
    } catch (err) {
        alert('Failed to rotate client key: ' + err);
    }
}
