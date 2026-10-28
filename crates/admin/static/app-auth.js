// LLMRoller Web UI — auth
// First-run setup, login, password change, and TOTP enrollment.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

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
