// LLMRoller Web UI — sessions
// Web sessions and the read-only chat-preview drawer.
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

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
