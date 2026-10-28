// LLMRoller Web UI — core
// Shared helpers, HTMX config, and the tab router.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the
// other app-*.js files: 49 of 54 functions are invoked by name from the delegated
// listeners in app-core.js, so per-file IIFEs would require rewriting every call
// site, which is a rewrite rather than the move this change is.
// CSP: script-src 'self'; no inline handlers.

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
    if (target) {
        target.classList.add('active');
        target.querySelectorAll('[hx-get]').forEach(el => {
            if (el.id) refreshPartial(el.id);
        });
    }
    if (tabId === 'tab-adm-settings' || tabId === 'tab-adm-traceability') loadAdminSettings();
    else if (tabId === 'tab-adm-catalog') loadAdminAliases();
    else if (tabId === 'tab-adm-price-book') loadAdminPrices();
    else if (tabId === 'tab-adm-overview') loadUsageChart();
    else if (tabId === 'tab-overview') loadPersonalOverviewAndChart();
    else if (tabId === 'tab-service-accounts') loadServiceAccounts();
    else if (tabId === 'tab-account-security') {
        syncUserPreferenceInputs();
        loadWebSessions();
    }
}

async function refreshPartial(id) {
    const el = typeof id === 'string' ? document.getElementById(id) : id;
    if (!el) return;
    const url = el.getAttribute('hx-get');
    if (!url) return;
    try {
        const res = await fetch(url, {
            headers: {
                'HX-Request': 'true',
                'Cache-Control': 'no-cache'
            }
        });
        if (!res.ok) return;
        const html = await res.text();
        el.innerHTML = html;
        if (typeof htmx !== 'undefined' && htmx.process) {
            htmx.process(el);
        }
    } catch (_e) {}
}

async function invalidateAndRefetch(scope) {
    const tasks = [];
    if (!scope || scope === 'all' || scope === 'providers' || scope === 'keys') {
        tasks.push(
            refreshPartial('providers-list'),
            refreshPartial('adm-providers-list'),
            refreshPartial('matrix-body'),
            refreshPartial('adm-models-body'),
            refreshPartial('admin-overview-container'),
            refreshPartial('user-audit-body'),
            refreshPartial('adm-audit-body'),
            loadPersonalOverviewAndChart(),
            loadAdminAliases()
        );
    }
    if (!scope || scope === 'all' || scope === 'client-keys') {
        tasks.push(
            refreshPartial('client-keys-body'),
            refreshPartial('user-audit-body'),
            loadPersonalOverviewAndChart()
        );
    }
    if (!scope || scope === 'all' || scope === 'users') {
        tasks.push(
            refreshPartial('adm-users-body'),
            refreshPartial('admin-overview-container'),
            refreshPartial('adm-audit-body')
        );
    }
    if (!scope || scope === 'all' || scope === 'shares') {
        tasks.push(
            refreshPartial('outgoing-shares-body'),
            refreshPartial('incoming-shares-body'),
            refreshPartial('matrix-body'),
            refreshPartial('user-audit-body')
        );
    }
    if (!scope || scope === 'all' || scope === 'catalog') {
        tasks.push(
            refreshPartial('adm-models-body'),
            refreshPartial('matrix-body'),
            refreshPartial('adm-audit-body'),
            loadAdminAliases()
        );
    }
    if (!scope || scope === 'all' || scope === 'prices') {
        tasks.push(
            loadAdminPrices(),
            refreshPartial('admin-overview-container'),
            refreshPartial('adm-audit-body')
        );
    }
    if (!scope || scope === 'all' || scope === 'pins') {
        tasks.push(
            refreshPartial('active-pins-body'),
            loadPersonalOverviewAndChart()
        );
    }
    await Promise.allSettled(tasks);
}

function openModal(id) {
    const modal = document.getElementById(id);
    if (modal) modal.classList.add('open');
}

function closeModal(id) {
    const modal = document.getElementById(id);
    if (modal) modal.classList.remove('open');
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

    // 13. Client Key Pause / Resume / Rotate
    const pauseKeyBtn = e.target.closest('[data-action="pause-client-key"]');
    if (pauseKeyBtn) {
        toggleClientKeyPause(pauseKeyBtn.getAttribute('data-id'), true);
        return;
    }
    const resumeKeyBtn = e.target.closest('[data-action="resume-client-key"]');
    if (resumeKeyBtn) {
        toggleClientKeyPause(resumeKeyBtn.getAttribute('data-id'), false);
        return;
    }
    const rotateKeyBtn = e.target.closest('[data-action="rotate-client-key"]');
    if (rotateKeyBtn) {
        rotateClientKey(rotateKeyBtn.getAttribute('data-id'));
        return;
    }

    // 14. Accept Incoming Share
    const acceptShareBtn = e.target.closest('[data-action="accept-share"]');
    if (acceptShareBtn) {
        acceptShare(
            acceptShareBtn.getAttribute('data-id'),
            acceptShareBtn.getAttribute('data-fingerprint')
        );
        return;
    }

    // 15. Confirm Model Merge
    const confirmMergeBtn = e.target.closest('[data-action="confirm-merge"]');
    if (confirmMergeBtn) {
        confirmModelMerge(confirmMergeBtn.getAttribute('data-id'));
        return;
    }

    // 16. Delete Price Book Entry
    const deletePriceBtn = e.target.closest('[data-action="delete-price"]');
    if (deletePriceBtn) {
        deletePriceEntry(
            deletePriceBtn.getAttribute('data-provider-id') || '',
            deletePriceBtn.getAttribute('data-model-id') || ''
        );
        return;
    }

    // 17. Delete Service Account
    const deleteSaBtn = e.target.closest('[data-action="delete-service-account"]');
    if (deleteSaBtn) {
        deleteServiceAccount(deleteSaBtn.getAttribute('data-id'));
        return;
    }

    // 18. Segmented Pill Active Button Toggle
    const segBtn = e.target.closest('.segmented-pill .btn');
    if (segBtn) {
        const pill = segBtn.closest('.segmented-pill');
        if (pill) {
            pill.querySelectorAll('.btn').forEach(b => {
                b.classList.remove('btn-primary');
                b.classList.add('btn-outline');
            });
            segBtn.classList.remove('btn-outline');
            segBtn.classList.add('btn-primary');
        }
    }

    // 19. Connection Setup Format Switcher (SRDD 14.h)
    const fmtBtn = e.target.closest('[data-action="select-api-format"]');
    if (fmtBtn) {
        selectApiFormat(fmtBtn.getAttribute('data-format') || 'chat', fmtBtn);
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
    if (
        e.target.id === 'prov-kind' ||
        e.target.id === 'gp-kind' ||
        e.target.id === 'prov-preset-id' ||
        e.target.id === 'provider-kind-select'
    ) {
        onProviderKindChange(e.target.value, e.target.id);
    }
});

// ---------------------------------------------------------------------------
// Async Handlers
// ---------------------------------------------------------------------------
async function extractApiError(res) {
    const body = await res.json().catch(() => ({}));
    return body.error?.message || body.error || `HTTP ${res.status}`;
}

let usageChart = null;

window.addEventListener('resize', () => {
    const container = document.getElementById('uplot-chart');
    if (usageChart && container) {
        usageChart.setSize({ width: container.clientWidth || 640, height: 240 });
    }
});

document.addEventListener('DOMContentLoaded', () => {
    initConnectionSetupUrls();
    loadAdminSettings();
    loadAdminAliases();
    loadAdminPrices();
    loadUsageChart();
    loadPersonalOverviewAndChart();
    loadServiceAccounts();
    syncUserPreferenceInputs();
    loadWebSessions();

    document.body.addEventListener('htmx:afterRequest', e => {
        const verb = (e.detail?.requestConfig?.verb || '').toLowerCase();
        if (!verb || verb === 'get') return;
        const path = e.detail?.pathInfo?.requestPath || '';
        if (path.includes('/providers') || path.includes('/upstream-keys')) {
            invalidateAndRefetch('providers');
        } else if (path.includes('/client-keys')) {
            invalidateAndRefetch('client-keys');
        } else if (path.includes('/users')) {
            invalidateAndRefetch('users');
        } else if (path.includes('/shares')) {
            invalidateAndRefetch('shares');
        } else if (path.includes('/models')) {
            invalidateAndRefetch('catalog');
        } else if (path.includes('/pins') || path.includes('/sessions')) {
            invalidateAndRefetch('pins');
        }
    });
});
