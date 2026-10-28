// LLMRoller Web UI — charts
// uPlot usage charts (admin and personal).
//
// Extracted verbatim from app.js (ISSUE-0013). Shares one global scope with the other
// app-*.js files: 53 of 54 functions are invoked by name from the delegated listeners in
// app-core.js, so per-file IIFEs would require rewriting every call site — a rewrite, not
// the extraction this was. Load order is fixed by APP_JS_MODULES and mirrored in the
// templates; a test asserts they agree.
// CSP: script-src 'self'; no inline handlers.

async function loadUsageChart() {
    const container = document.getElementById('uplot-chart');
    if (!container || typeof uPlot === 'undefined') return;
    try {
        const [summaryRes, eventsRes] = await Promise.all([
            fetch('/admin/api/usage/summary'),
            fetch('/admin/api/usage/events?limit=500')
        ]);
        const summary = summaryRes.ok ? await summaryRes.json() : {};
        const eventPayload = eventsRes.ok ? await eventsRes.json() : { events: [] };
        const byDay = new Map();
        for (const event of eventPayload.events || []) {
            const day = Math.floor(Number(event.ts || 0) / 86_400_000) * 86_400_000;
            byDay.set(day, (byDay.get(day) || 0) + Number(event.cost_micro_usd || 0));
        }
        let xs = [...byDay.keys()].sort((a, b) => a - b).map(ms => Math.floor(ms / 1000));
        let ys = xs.map(seconds => (byDay.get(seconds * 1000) || 0) / 1_000_000);
        if (xs.length === 0) {
            xs = [Math.floor(Date.now() / 1000)];
            ys = [Number(summary.total_cost_micro_usd || 0) / 1_000_000];
        }

        const summaryEl = document.getElementById('uplot-summary');
        if (summaryEl) {
            summaryEl.textContent = `${Number(summary.total_requests || 0).toLocaleString()} requests • $${(Number(summary.total_cost_micro_usd || 0) / 1_000_000).toFixed(4)} total spend`;
        }

        if (usageChart) {
            usageChart.destroy();
            usageChart = null;
        }
        container.innerHTML = '';
        container.classList.remove('hidden');
        usageChart = new uPlot({
            width: container.clientWidth || 640,
            height: 240,
            scales: { x: { time: true } },
            series: [
                {},
                {
                    label: 'USD',
                    stroke: '#38bdf8',
                    width: 2,
                    fill: 'rgba(56, 189, 248, 0.12)'
                }
            ],
            axes: [
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } },
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } }
            ]
        }, [xs, ys], container);
    } catch (err) {
        container.innerHTML = `<span class="text-muted">Usage chart unavailable: ${escapeHtml(err)}</span>`;
    }
}

let personalUsageChart = null;

async function loadPersonalOverviewAndChart() {
    const keysEl = document.getElementById('stat-keys-count');
    const provEl = document.getElementById('stat-providers-count');
    const sessEl = document.getElementById('stat-sessions-count');
    if (keysEl || provEl || sessEl) {
        try {
            const [keysRes, provRes, pinsRes] = await Promise.all([
                fetch('/api/user/client-keys'),
                fetch('/api/user/providers'),
                fetch('/api/user/pins')
            ]);
            if (keysEl && keysRes.ok) {
                const keys = await keysRes.json();
                keysEl.textContent = String(Array.isArray(keys) ? keys.length : 0);
            }
            if (provEl && provRes.ok) {
                const provs = await provRes.json();
                provEl.textContent = String(Array.isArray(provs) ? provs.length : 0);
            }
            if (sessEl && pinsRes.ok) {
                const pins = await pinsRes.json();
                sessEl.textContent = String(Array.isArray(pins) ? pins.length : 0);
            }
        } catch (_e) {}
    }

    const container = document.getElementById('personal-usage-chart');
    if (!container || typeof uPlot === 'undefined') return;
    try {
        const [summaryRes, seriesRes] = await Promise.all([
            fetch('/api/user/usage/summary'),
            fetch('/api/user/usage/timeseries')
        ]);
        const summary = summaryRes.ok ? await summaryRes.json() : {};
        const seriesData = seriesRes.ok ? await seriesRes.json() : { series: [] };
        const pts = seriesData.series || [];
        let xs = pts.map(p => Math.floor(Number(p.bucket_ms || 0) / 1000));
        let ys = pts.map(p => Number(p.cost_micro_usd || 0) / 1_000_000);
        if (xs.length === 0) {
            xs = [Math.floor(Date.now() / 1000)];
            ys = [Number(summary.total_cost_micro_usd || 0) / 1_000_000];
        }
        const summaryEl = document.getElementById('personal-uplot-summary');
        if (summaryEl) {
            summaryEl.textContent = `${Number(summary.total_requests || 0).toLocaleString()} requests • $${(Number(summary.total_cost_micro_usd || 0) / 1_000_000).toFixed(4)} personal spend`;
        }
        if (personalUsageChart) {
            personalUsageChart.destroy();
            personalUsageChart = null;
        }
        container.innerHTML = '';
        personalUsageChart = new uPlot({
            width: container.clientWidth || 640,
            height: 240,
            scales: { x: { time: true } },
            series: [{}, { label: 'USD', stroke: '#38bdf8', width: 2, fill: 'rgba(56, 189, 248, 0.12)' }],
            axes: [
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } },
                { stroke: '#94a3b8', grid: { stroke: 'rgba(148, 163, 184, 0.15)' } }
            ]
        }, [xs, ys], container);
    } catch (err) {
        container.innerHTML = `<span class="text-muted">Personal usage chart unavailable: ${escapeHtml(err)}</span>`;
    }
}
