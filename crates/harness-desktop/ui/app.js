// Dex.app Application Controller (Phase 10 — Zero-Emoji, Full IPC Parity)
(function (global) {
  'use strict';

  const invoke = global.__TAURI__
    ? global.__TAURI__.core.invoke
    : async () => { throw new Error('Tauri runtime unavailable'); };
  const listen = global.__TAURI__ ? global.__TAURI__.event.listen : () => {};
  const icon = (name, size, cls) => global.DexIcons.svg(name, size || 14, cls || '');

  const VIEW_TITLES = {
    'view-agent': 'Agent',
    'view-media': 'Media Studio',
    'view-audio': 'Audio & Documents',
    'view-3d': '3D & Game Studio',
    'view-models': 'Models Hub',
    'view-extensions': 'Extensions & Sidecars',
    'view-cookbook': 'Hardware Cookbook',
  };

  const state = {
    activeView: 'view-agent',
    inspectorOpen: false,
    inspectorTab: 'tasks',
    sidebarCollapsed: false,
    running: false,
    mode: 'autopilot',
    isRecordingVoice: false,
    lastParsedDoc: null,
    activeExtCategory: 'all',
    cachedExtCatalog: [],
    cachedInstalledExts: [],
    visualMediaCache: [],
    models3dCache: [],
    selected3dModel: null,
    selected3dReport: null,
    glbViewer: null,
    // Structured Agent Chat Stream State
    currentThoughtEl: null,
    activeToolCards: [],
    streamedChars: 0,
    subagentEntries: [],
    // Models Hub (Phase 11 — Bionic / LM Studio Style)
    modelsTab: 'library',
    modelDirs: [],
    localServers: [],
    scannedModels: [],
    libraryFormat: 'all',
    selectedGroupVariant: {},
    hfReposCache: [],
    selectedHfRepoId: null,
    selectedHfFiles: [],
    exploreInitialized: false,
  };

  function el(id) {
    return document.getElementById(id);
  }

  function escapeHtml(str) {
    return String(str === null || str === undefined ? '' : str)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function formatDuration(ms) {
    if (ms < 1000) return `${Math.max(1, Math.round(ms))}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  }

  function formatBytesKb(bytes) {
    return `${Math.max(0.1, bytes / 1024).toFixed(1)} KB`;
  }

  // =========================================================================
  // Navigation, Layout Toggles & Keyboard Shortcuts
  // =========================================================================
  function navigate(viewId) {
    if (!VIEW_TITLES[viewId]) return;
    state.activeView = viewId;

    document.querySelectorAll('.nav-item').forEach((btn) => {
      btn.classList.toggle('active', btn.getAttribute('data-view') === viewId);
    });
    document.querySelectorAll('.view-panel').forEach((panel) => {
      panel.classList.toggle('active', panel.id === viewId);
    });

    const titleEl = el('titlebar-view-title');
    if (titleEl) titleEl.textContent = VIEW_TITLES[viewId];

    if (viewId === 'view-media') {
      refreshComfyWorkflows();
      refreshVisualMedia();
    } else if (viewId === 'view-audio') {
      refreshAudioMedia();
    } else if (viewId === 'view-3d') {
      init3dStudioView();
    } else if (viewId === 'view-models') {
      initModelsHubView();
    } else if (viewId === 'view-extensions') {
      refreshExtensions();
    } else if (viewId === 'view-cookbook') {
      refreshCookbook();
    }
  }

  function toggleSidebar() {
    state.sidebarCollapsed = !state.sidebarCollapsed;
    const sb = el('sidebar');
    const btn = el('btn-toggle-sidebar');
    if (sb) sb.classList.toggle('collapsed', state.sidebarCollapsed);
    if (btn) btn.classList.toggle('active', state.sidebarCollapsed);
    if (state.glbViewer) {
      setTimeout(() => { state.glbViewer.resize(); state.glbViewer.render(); }, 220);
    }
  }

  function setInspectorOpen(open) {
    state.inspectorOpen = !!open;
    const drawer = el('inspector-drawer');
    const btn = el('btn-toggle-inspector');
    if (drawer) drawer.classList.toggle('closed', !state.inspectorOpen);
    if (btn) btn.classList.toggle('active', state.inspectorOpen);
    if (state.inspectorOpen) {
      if (state.inspectorTab === 'tasks') refreshTasks();
      if (state.inspectorTab === 'vision') refreshScreenshots();
    }
    if (state.glbViewer) {
      setTimeout(() => { state.glbViewer.resize(); state.glbViewer.render(); }, 220);
    }
  }

  function toggleInspector() {
    setInspectorOpen(!state.inspectorOpen);
  }

  function switchInspector(tab) {
    state.inspectorTab = tab;
    document.querySelectorAll('.inspector-seg .seg-btn').forEach((b) => {
      b.classList.toggle('active', b.getAttribute('data-inspector') === tab);
    });
    document.querySelectorAll('.inspector-pane').forEach((p) => {
      p.classList.toggle('active', p.id === `inspector-${tab}`);
    });
    if (tab === 'tasks') refreshTasks();
    if (tab === 'vision') refreshScreenshots();
  }

  // =========================================================================
  // Model Popover & Composer Helpers
  // =========================================================================
  function toggleModelPopover() {
    const pop = el('model-popover');
    if (pop) pop.classList.toggle('open');
  }

  function closeModelPopover() {
    const pop = el('model-popover');
    if (pop) pop.classList.remove('open');
  }

  function openModelConfig() {
    navigate('view-agent');
    const pop = el('model-popover');
    if (pop) pop.classList.add('open');
  }

  function syncModelLabels() {
    const provider = (el('sel-provider') && el('sel-provider').value) || 'ollama';
    const model = (el('input-model') && el('input-model').value.trim()) || 'qwen2.5:7b';
    const text = `${provider} / ${model}`;
    const cLabel = el('composer-model-label');
    const sLabel = el('sidebar-model-label');
    if (cLabel) cLabel.textContent = text;
    if (sLabel) sLabel.textContent = text;
  }

  function onProviderChange() {
    const provider = el('sel-provider').value;
    const modelInput = el('input-model');
    const apiBaseInput = el('input-apibase');
    if (provider === 'ollama') {
      if (modelInput.value === 'gpt-4o') modelInput.value = 'qwen2.5:7b';
      if (apiBaseInput && apiBaseInput.value === 'http://127.0.0.1:1234/v1') apiBaseInput.value = '';
    } else if (provider === 'lmstudio') {
      if (apiBaseInput && !apiBaseInput.value.trim()) {
        apiBaseInput.value = 'http://127.0.0.1:1234/v1';
      }
    } else if (provider === 'openai') {
      if (modelInput.value === 'qwen2.5:7b') modelInput.value = 'gpt-4o';
      if (apiBaseInput && apiBaseInput.value === 'http://127.0.0.1:1234/v1') apiBaseInput.value = '';
    }
    syncModelLabels();
  }

  function autoResizeComposer() {
    const ta = el('input-goal');
    if (!ta) return;
    ta.style.height = 'auto';
    ta.style.height = `${Math.min(168, Math.max(24, ta.scrollHeight))}px`;
  }

  function useQuickPrompt(prompt) {
    navigate('view-agent');
    const ta = el('input-goal');
    if (!ta) return;
    ta.value = prompt;
    autoResizeComposer();
    ta.focus();
  }

  function sendPromptToAgent(prompt, noteText) {
    navigate('view-agent');
    const ta = el('input-goal');
    if (ta) {
      ta.value = prompt;
      autoResizeComposer();
      ta.focus();
    }
    if (noteText) appendSystemNote(noteText);
  }

  // =========================================================================
  // Structured Agent Feed (Thought Stream + Collapsible Tool Cards)
  // =========================================================================
  function hideEmptyState() {
    const empty = el('agent-empty');
    if (empty) empty.style.display = 'none';
  }

  function scrollFeedToBottom() {
    const scroller = el('agent-feed-scroll');
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }

  function clearAgentFeed() {
    const feed = el('agent-feed');
    if (!feed) return;
    const empty = el('agent-empty');
    Array.from(feed.children).forEach((child) => {
      if (child.id !== 'agent-empty') child.remove();
    });
    if (empty) empty.style.display = 'flex';
    state.currentThoughtEl = null;
    state.activeToolCards = [];
    state.streamedChars = 0;
    updateContextMetric(0);
  }

  function updateContextMetric(estimatedTokens) {
    const ctxEl = el('sidebar-ctx-metric');
    if (!ctxEl) return;
    if (estimatedTokens >= 1000) {
      ctxEl.textContent = `${(estimatedTokens / 1000).toFixed(1)}k tok`;
    } else {
      ctxEl.textContent = `${estimatedTokens} tok`;
    }
  }

  function appendUserMessage(text) {
    hideEmptyState();
    state.currentThoughtEl = null;
    const feed = el('agent-feed');
    const div = document.createElement('div');
    div.className = 'chat-entry';
    div.innerHTML = `<div class="chat-user">${escapeHtml(text)}</div>`;
    feed.appendChild(div);
    scrollFeedToBottom();
  }

  function appendThoughtToken(token) {
    hideEmptyState();
    const feed = el('agent-feed');
    if (!state.currentThoughtEl) {
      const wrap = document.createElement('div');
      wrap.className = 'chat-entry chat-assistant-block';
      const p = document.createElement('div');
      p.className = 'chat-thought';
      wrap.appendChild(p);
      feed.appendChild(wrap);
      state.currentThoughtEl = p;
    }
    state.currentThoughtEl.textContent += token;
    state.streamedChars += token.length;
    updateContextMetric(Math.round(state.streamedChars / 4));
    scrollFeedToBottom();
  }

  function appendToolStartedCard(toolName, args) {
    hideEmptyState();
    state.currentThoughtEl = null;
    const feed = el('agent-feed');
    const card = document.createElement('div');
    card.className = 'tool-card';
    const argsStr = typeof args === 'string' ? args : JSON.stringify(args || {}, null, 2);
    card.innerHTML = `
      <div class="tool-card-head">
        <div class="tool-card-left">
          <span class="tool-card-chevron">${icon('chevronRight', 13)}</span>
          ${icon('wrench', 13)}
          <span class="tool-card-name">${escapeHtml(toolName)}</span>
        </div>
        <div class="tool-card-right">
          <span class="tool-duration">running</span>
          <span class="badge badge-accent tool-status-badge">Running</span>
        </div>
      </div>
      <div class="tool-card-body">
        <div class="tool-section-label">Arguments</div>
        <pre class="tool-pre tool-args-pre">${escapeHtml(argsStr)}</pre>
        <div class="tool-section-label tool-out-label" style="display: none;">Output</div>
        <pre class="tool-pre tool-out-pre" style="display: none;"></pre>
      </div>
    `;
    const head = card.querySelector('.tool-card-head');
    head.addEventListener('click', () => card.classList.toggle('open'));
    feed.appendChild(card);

    state.activeToolCards.push({
      toolName,
      el: card,
      startedAt: performance.now(),
    });
    scrollFeedToBottom();
  }

  function resolveToolFinishedCard(toolName, output, isError) {
    let idx = -1;
    for (let i = state.activeToolCards.length - 1; i >= 0; i--) {
      if (state.activeToolCards[i].toolName === toolName) {
        idx = i;
        break;
      }
    }
    if (idx === -1) {
      appendToolStartedCard(toolName, {});
      idx = state.activeToolCards.length - 1;
    }

    const rec = state.activeToolCards.splice(idx, 1)[0];
    const elapsed = performance.now() - rec.startedAt;
    const card = rec.el;

    const durEl = card.querySelector('.tool-duration');
    const badgeEl = card.querySelector('.tool-status-badge');
    const outLabel = card.querySelector('.tool-out-label');
    const outPre = card.querySelector('.tool-out-pre');

    if (durEl) durEl.textContent = formatDuration(elapsed);
    if (badgeEl) {
      badgeEl.className = `badge tool-status-badge ${isError ? 'badge-err' : 'badge-ok'}`;
      badgeEl.textContent = isError ? 'Failed' : 'Done';
    }
    if (outLabel) outLabel.style.display = 'block';
    if (outPre) {
      outPre.style.display = 'block';
      const text = String(output || '');
      outPre.textContent = text.length > 4000 ? `${text.slice(0, 4000)}\n... (${text.length} chars total)` : text;
    }
    if (isError) card.classList.add('open');
    scrollFeedToBottom();
  }

  function appendFinalAnswer(answer) {
    hideEmptyState();
    state.currentThoughtEl = null;
    const feed = el('agent-feed');
    const div = document.createElement('div');
    div.className = 'chat-entry';
    div.innerHTML = `
      <div class="chat-final-answer">
        <div class="chat-final-label">${icon('checkCircle', 13)} Completed</div>
        <div>${escapeHtml(answer)}</div>
      </div>
    `;
    feed.appendChild(div);
    scrollFeedToBottom();
  }

  function appendSystemNote(text, variant) {
    hideEmptyState();
    state.currentThoughtEl = null;
    const feed = el('agent-feed');
    const div = document.createElement('div');
    div.className = 'chat-entry';
    const iconName = variant === 'error' ? 'xCircle' : variant === 'warn' ? 'alertTriangle' : 'activity';
    const cls = variant ? `chat-system-note ${variant}` : 'chat-system-note';
    div.innerHTML = `<div class="${cls}">${icon(iconName, 13)} <span>${escapeHtml(text)}</span></div>`;
    feed.appendChild(div);
    scrollFeedToBottom();
  }

  // =========================================================================
  // Agent Execution, Modes & Safe Mode Approval
  // =========================================================================
  function setRunningState(running) {
    state.running = running;
    const runBtn = el('btn-run');
    const stopBtn = el('btn-stop');
    const pill = el('agent-status-pill');
    const pillText = el('agent-status-text');

    if (runBtn) runBtn.style.display = running ? 'none' : 'inline-flex';
    if (stopBtn) stopBtn.style.display = running ? 'inline-flex' : 'none';
    if (pill) pill.classList.toggle('running', running);
    if (pillText) pillText.textContent = running ? 'Running' : 'Idle';
  }

  function updateModeButtons(modeStr) {
    const lower = String(modeStr || 'autopilot').toLowerCase();
    state.mode = lower;
    ['autopilot', 'safe', 'plan'].forEach((m) => {
      const btn = el(`mode-btn-${m}`);
      if (!btn) return;
      btn.className = 'seg-btn';
      if (m === lower) {
        btn.classList.add('active');
        if (m === 'safe') btn.classList.add('mode-safe');
        if (m === 'plan') btn.classList.add('mode-plan');
      }
    });
  }

  async function setMode(mode) {
    updateModeButtons(mode);
    try {
      await invoke('set_mode', { mode });
      appendSystemNote(`Execution mode switched to ${mode.toUpperCase()}`);
    } catch (err) {
      appendSystemNote(`Failed to set mode: ${err}`, 'error');
    }
  }

  async function runGoal() {
    const ta = el('input-goal');
    if (!ta) return;
    const goal = ta.value.trim();
    if (!goal || state.running) return;

    const provider = el('sel-provider').value;
    const model = el('input-model').value.trim();
    const apiBase = el('input-apibase').value.trim() || null;

    closeModelPopover();
    appendUserMessage(goal);
    ta.value = '';
    autoResizeComposer();
    setRunningState(true);

    try {
      await invoke('run_goal', {
        goal,
        mode: state.mode,
        provider,
        model,
        apiBase,
      });
    } catch (err) {
      appendSystemNote(`Failed to launch agent: ${err}`, 'error');
      setRunningState(false);
    }
  }

  async function stopGoal() {
    try {
      await invoke('stop_goal');
      appendSystemNote('Cancellation requested by user', 'warn');
    } catch (err) {
      appendSystemNote(`Stop error: ${err}`, 'error');
    }
  }

  function showApprovalModal(toolName, args) {
    el('modal-tool-name').textContent = toolName;
    el('modal-arguments').textContent =
      typeof args === 'string' ? args : JSON.stringify(args || {}, null, 2);
    el('approval-modal').classList.add('open');
  }

  async function resolveApproval(approved) {
    el('approval-modal').classList.remove('open');
    try {
      await invoke('resolve_approval', { approved });
      appendSystemNote(
        approved ? 'Safe Mode action approved by user' : 'Safe Mode action denied by user',
        approved ? '' : 'warn'
      );
    } catch (err) {
      appendSystemNote(`Approval error: ${err}`, 'error');
    }
  }

  // =========================================================================
  // Inspector: TaskBoard, Subagents & Vision Screenshots
  // =========================================================================
  async function refreshTasks() {
    const container = el('tasks-list');
    if (!container) return;
    try {
      const tasks = await invoke('get_tasks');
      if (!tasks || tasks.length === 0) {
        container.innerHTML = '<div class="empty-note">No tasks found in .dex/tasks.json.</div>';
        return;
      }
      container.innerHTML = tasks.map((t) => {
        const statusStr = typeof t.status === 'string' ? t.status : Object.keys(t.status || {})[0] || 'pending';
        const badgeCls =
          statusStr === 'completed' ? 'badge-ok'
          : statusStr === 'in_progress' ? 'badge-accent'
          : statusStr === 'blocked' ? 'badge-err'
          : '';
        return `
          <div class="item-card">
            <div class="item-card-row">
              <span class="item-title" style="font-size: 12px;">${escapeHtml(t.id)}: ${escapeHtml(t.title)}</span>
              <span class="badge ${badgeCls}">${escapeHtml(statusStr)}</span>
            </div>
            <div class="item-desc">${escapeHtml(t.description || '')}</div>
          </div>
        `;
      }).join('');
    } catch (err) {
      container.innerHTML = `<div class="empty-note">${escapeHtml(err)}</div>`;
    }
  }

  function recordSubagentSpawn(role, task) {
    state.subagentEntries.unshift({ role, task, status: 'running' });
    renderSubagents();
  }

  function recordSubagentComplete(role) {
    const match = state.subagentEntries.find((s) => s.role === role && s.status === 'running');
    if (match) match.status = 'completed';
    renderSubagents();
  }

  function renderSubagents() {
    const container = el('subagents-list');
    if (!container) return;
    if (state.subagentEntries.length === 0) {
      container.innerHTML = '<div class="empty-note">No subagents spawned in this session.</div>';
      return;
    }
    container.innerHTML = state.subagentEntries.map((s) => `
      <div class="item-card">
        <div class="item-card-row">
          <span class="item-title" style="font-size: 12px;">${icon('gitBranch', 12)} ${escapeHtml(s.role)}</span>
          <span class="badge ${s.status === 'completed' ? 'badge-ok' : 'badge-accent'}">${s.status === 'completed' ? 'Done' : 'Running'}</span>
        </div>
        <div class="item-desc">${escapeHtml(s.task)}</div>
      </div>
    `).join('');
  }

  async function refreshScreenshots() {
    const container = el('gallery-container');
    if (!container) return;
    try {
      const screenshots = await invoke('get_screenshots');
      if (!screenshots || screenshots.length === 0) {
        container.innerHTML = '<div class="empty-note" style="grid-column: 1 / -1;">No screenshots captured yet.</div>';
        return;
      }
      container.innerHTML = screenshots.map((s) => `
        <div class="screenshot-card" onclick="window.open('${s.data_url}')">
          <img src="${s.data_url}" alt="${escapeHtml(s.filename)}" />
          <div class="screenshot-card-meta">${escapeHtml(s.filename)} (${Math.round(s.size_bytes / 1024)} KB)</div>
        </div>
      `).join('');
    } catch (err) {
      container.innerHTML = `<div class="empty-note" style="grid-column: 1 / -1;">${escapeHtml(err)}</div>`;
    }
  }

  // =========================================================================
  // Hardware Cookbook & Sidebar Metrics (Fixed JSON field mapping)
  // =========================================================================
  async function refreshCookbook() {
    try {
      const res = await invoke('get_cookbook');
      if (!res) return;
      const reportEl = el('cookbook-report');
      if (reportEl && res.report) reportEl.textContent = res.report;

      const p = res.profile || {};
      const totalMem = p.total_memory_gb !== undefined ? p.total_memory_gb : p.total_ram_gb;
      const availMem = p.available_memory_gb !== undefined ? p.available_memory_gb : p.available_ram_gb;
      const ramGb = totalMem !== undefined ? `${Number(totalMem).toFixed(1)} GB` : 'n/a';
      const availGb = availMem !== undefined ? `${Number(availMem).toFixed(1)} GB free` : '';
      const sidebarRam = el('sidebar-ram-metric');
      if (sidebarRam) sidebarRam.textContent = ramGb;

      const statsEl = el('cookbook-stats');
      if (statsEl) {
        const topRec = (res.recommendations && res.recommendations[0])
          ? res.recommendations[0].tier || res.recommendations[0].model_name
          : 'Detected';
        const platformLabel = p.is_apple_silicon ? 'Apple Silicon' : (p.os_name || 'Desktop');
        statsEl.innerHTML = `
          <div class="stat-tile">
            <span class="stat-label">Total Memory</span>
            <span class="stat-value mono">${escapeHtml(ramGb)}</span>
            <span class="card-meta">${escapeHtml(availGb || (p.is_apple_silicon ? 'Unified Memory' : 'System RAM'))}</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">CPU Cores</span>
            <span class="stat-value mono">${escapeHtml(p.cpu_cores || 'n/a')}</span>
            <span class="card-meta">${escapeHtml(p.os_name || 'Host OS')} · ${escapeHtml(p.arch || '')}</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">Platform</span>
            <span class="stat-value">${escapeHtml(platformLabel)}</span>
            <span class="card-meta mono">${p.is_apple_silicon ? 'Unified GPU / MLX Ready' : escapeHtml(p.arch || '')}</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">Recommended Tier</span>
            <span class="stat-value">${escapeHtml(topRec)}</span>
            <span class="card-meta">${(res.recommendations || []).length} model profile(s)</span>
          </div>
        `;
      }
    } catch (err) {
      const reportEl = el('cookbook-report');
      if (reportEl) reportEl.textContent = `Hardware detection error: ${err}`;
    }
  }

  // =========================================================================
  // Models Hub (Phase 11 — Bionic / LM Studio: Library + Explore Master-Detail)
  // =========================================================================
  function formatSizeGb(bytes) {
    if (!bytes || bytes <= 0) return '-';
    const gb = bytes / (1024 * 1024 * 1024);
    if (gb >= 1.0) return `${gb.toFixed(2)} GB`;
    const mb = bytes / (1024 * 1024);
    return `${mb.toFixed(0)} MB`;
  }

  function formatRelativeDate(isoStr) {
    if (!isoStr) return '';
    const ms = Date.parse(isoStr);
    if (Number.isNaN(ms)) return '';
    const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
    if (diffSec < 3600) return `${Math.max(1, Math.floor(diffSec / 60))}m ago`;
    if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
    const days = Math.floor(diffSec / 86400);
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }

  function renderCapabilityChips(caps) {
    if (!caps || caps.length === 0) return '';
    return caps.map((c) => {
      const ic = c === 'Vision' ? 'eye' : c === 'Reasoning' ? 'brain' : 'wrench';
      return `<span class="cap-chip" title="${escapeHtml(c)}">${icon(ic, 11)}<span>${escapeHtml(c)}</span></span>`;
    }).join('');
  }

  function formatBadgeClass(fmt) {
    const f = String(fmt || '').toLowerCase();
    if (f === 'mlx') return 'badge-accent';
    if (f === 'ollama') return 'badge-ok';
    if (f === 'pt') return 'badge-warn';
    return '';
  }

  async function initModelsHubView() {
    await refreshModelsLibrary();
    if (state.modelsTab === 'explore' && !state.exploreInitialized) {
      state.exploreInitialized = true;
      searchHfModels();
    }
  }

  function switchModelsTab(tab) {
    state.modelsTab = tab;
    const isLib = tab === 'library';
    el('models-tab-library').classList.toggle('active', isLib);
    el('models-tab-explore').classList.toggle('active', !isLib);
    el('models-sub-library').classList.toggle('active', isLib);
    el('models-sub-explore').classList.toggle('active', !isLib);
    const actions = el('models-topbar-actions');
    if (actions) actions.style.display = isLib ? 'flex' : 'none';

    if (!isLib && !state.exploreInitialized) {
      state.exploreInitialized = true;
      searchHfModels();
    }
  }

  function closeModelsDrawers() {
    ['drawer-add-folder', 'drawer-pull-ollama', 'drawer-import-model'].forEach((id) => {
      const node = el(id);
      if (node) node.classList.remove('open');
    });
  }

  function toggleModelsDrawer(kind) {
    const map = {
      folder: 'drawer-add-folder',
      ollama: 'drawer-pull-ollama',
      import: 'drawer-import-model',
    };
    const targetId = map[kind];
    if (!targetId) return;
    const target = el(targetId);
    const wasOpen = target && target.classList.contains('open');
    closeModelsDrawers();
    if (target && !wasOpen) {
      target.classList.add('open');
      const inp = target.querySelector('input');
      if (inp) inp.focus();
    }
  }

  async function refreshInstalledModels() {
    // Keep backward-compatible IPC call + full multi-folder refresh
    try { await invoke('list_installed_models'); } catch (_) {}
    await refreshModelsLibrary();
  }

  async function refreshModelsLibrary() {
    const listEl = el('library-models-list');
    if (listEl && state.scannedModels.length === 0) {
      listEl.innerHTML = '<div class="empty-note">Scanning model directories and local servers...</div>';
    }
    try {
      const [dirs, scanned, servers] = await Promise.all([
        invoke('list_model_dirs'),
        invoke('scan_local_models'),
        invoke('detect_local_servers'),
      ]);
      state.modelDirs = dirs || [];
      state.scannedModels = scanned || [];
      state.localServers = servers || [];
      renderModelDirsStrip();
      renderLocalServersStrip();
      renderLibraryModels();
    } catch (err) {
      if (listEl) listEl.innerHTML = `<div class="empty-note" style="color: var(--err-fg);">Scan error: ${escapeHtml(err)}</div>`;
    }
  }

  function renderModelDirsStrip() {
    const container = el('model-dirs-chips');
    if (!container) return;
    if (!state.modelDirs || state.modelDirs.length === 0) {
      container.innerHTML = '<span class="card-meta">No model folders configured.</span>';
      return;
    }
    container.innerHTML = state.modelDirs.map((d, idx) => `
      <span class="dir-chip" title="${escapeHtml(d.path)}">
        ${icon('folder', 12)}
        <span>${escapeHtml(d.path)}</span>
        ${d.exists ? '' : '<span class="badge badge-warn" style="padding: 0 4px; font-size: 9px;">missing</span>'}
        <button class="dir-chip-rm" title="Remove folder" onclick="DexApp.removeModelDirFromUi(${idx})">${icon('x', 11)}</button>
      </span>
    `).join('');
  }

  function renderLocalServersStrip() {
    const container = el('local-servers-strip');
    if (!container) return;
    if (!state.localServers || state.localServers.length === 0) {
      container.innerHTML = '';
      return;
    }
    container.innerHTML = state.localServers.map((srv, idx) => {
      const online = !!srv.reachable;
      const dotStyle = online ? 'background: var(--ok-fg);' : 'background: var(--text-muted);';
      const countText = online ? `${srv.loaded_models.length} loaded` : 'offline';
      const connectBtn = online && srv.loaded_models.length > 0
        ? `<button class="btn btn-sm" style="height: 22px; padding: 0 7px; font-size: 10.5px;" onclick="DexApp.connectLocalServer(${idx})">Connect</button>`
        : '';
      return `
        <div class="server-pill ${online ? 'online' : ''}" title="${escapeHtml(srv.base_url)}">
          <span class="status-dot" style="${dotStyle}"></span>
          ${icon('server', 12)}
          <span style="font-weight: 500;">${escapeHtml(srv.name)}</span>
          <span class="mono card-meta">${countText}</span>
          ${connectBtn}
        </div>
      `;
    }).join('');
  }

  function connectLocalServer(idx) {
    const srv = state.localServers[idx];
    if (!srv || !srv.loaded_models || srv.loaded_models.length === 0) return;
    const modelName = srv.loaded_models[0];
    el('sel-provider').value = srv.id === 'lmstudio' ? 'lmstudio' : srv.provider;
    el('input-model').value = modelName;
    el('input-apibase').value = srv.provider === 'openai' ? srv.base_url : '';
    syncModelLabels();
    navigate('view-agent');
    appendSystemNote(`Connected to ${srv.name} (${srv.base_url}) with model ${modelName}`);
    el('input-goal').focus();
  }

  async function addModelDirFromUi() {
    const inp = el('input-model-dir');
    const path = inp ? inp.value.trim() : '';
    if (!path) return alert('Enter a folder path (e.g. ~/.lmstudio/models).');
    try {
      await invoke('add_model_dir', { path });
      inp.value = '';
      closeModelsDrawers();
      await refreshModelsLibrary();
    } catch (err) {
      alert(`Could not add folder: ${err}`);
    }
  }

  async function removeModelDirFromUi(idx) {
    const entry = state.modelDirs[idx];
    if (!entry) return;
    try {
      await invoke('remove_model_dir', { path: entry.path });
      await refreshModelsLibrary();
    } catch (err) {
      alert(`Could not remove folder: ${err}`);
    }
  }

  function setLibraryFormat(fmt, btnEl) {
    state.libraryFormat = fmt;
    document.querySelectorAll('#lib-format-filter .seg-btn').forEach((b) => b.classList.remove('active'));
    if (btnEl) btnEl.classList.add('active');
    renderLibraryModels();
  }

  function selectLibraryVariant(groupId, modelId) {
    state.selectedGroupVariant[groupId] = modelId;
    renderLibraryModels();
  }

  function renderLibraryModels() {
    const container = el('library-models-list');
    if (!container) return;
    const q = (el('lib-search-input') && el('lib-search-input').value.trim().toLowerCase()) || '';
    const sortBy = (el('lib-sort-select') && el('lib-sort-select').value) || 'recent';

    let filtered = state.scannedModels.filter((m) => {
      if (state.libraryFormat !== 'all' && m.format !== state.libraryFormat) return false;
      if (!q) return true;
      const hay = `${m.name} ${m.quantization || ''} ${m.architecture || ''} ${m.repo_hint || ''} ${m.path || ''} ${m.format}`.toLowerCase();
      return hay.includes(q);
    });

    if (filtered.length === 0) {
      container.innerHTML = '<div class="empty-note">No models match the active filter. Add a model folder (e.g. ~/.lmstudio/models) or switch to Explore to download.</div>';
      return;
    }

    // Group variants of the same model family & format (e.g. 4bit + 8bit MLX)
    const groupMap = new Map();
    for (const m of filtered) {
      const gKey = `${m.format}:${m.group_key || m.id}`;
      if (!groupMap.has(gKey)) groupMap.set(gKey, []);
      groupMap.get(gKey).push(m);
    }

    const groups = Array.from(groupMap.entries()).map(([gKey, variants]) => {
      variants.sort((a, b) => (a.size_bytes || 0) - (b.size_bytes || 0));
      const chosenId = state.selectedGroupVariant[gKey];
      const active = variants.find((v) => v.id === chosenId) || variants[0];
      const newest = Math.max(...variants.map((v) => v.modified_at || 0));
      const largest = Math.max(...variants.map((v) => v.size_bytes || 0));
      return { gKey, variants, active, newest, largest };
    });

    if (sortBy === 'size') {
      groups.sort((a, b) => b.largest - a.largest);
    } else if (sortBy === 'name') {
      groups.sort((a, b) => a.active.name.toLowerCase().localeCompare(b.active.name.toLowerCase()));
    } else {
      groups.sort((a, b) => b.newest - a.newest || b.largest - a.largest);
    }

    container.innerHTML = groups.map((g) => {
      const m = g.active;
      const fmtLabel = m.format === 'pt' ? 'PT' : m.format.toUpperCase();
      const fmtBadge = `<span class="badge ${formatBadgeClass(m.format)} mono">${fmtLabel}</span>`;
      const variantHtml = g.variants.length > 1
        ? `<div class="variant-pills">${g.variants.map((v) => {
            const label = v.quantization || formatSizeGb(v.size_bytes);
            const isAct = v.id === m.id;
            return `<button class="variant-pill ${isAct ? 'active' : ''}" onclick="DexApp.selectLibraryVariant('${escapeHtml(g.gKey)}', '${escapeHtml(v.id)}')">${escapeHtml(label)}</button>`;
          }).join('')}</div>`
        : (m.quantization ? `<span class="badge mono">${escapeHtml(m.quantization)}</span>` : '');
      const capsHtml = renderCapabilityChips(m.capabilities);
      const subText = m.repo_hint || m.path || m.source_dir || '';
      const ctxBadge = m.context_length
        ? `<span class="card-meta mono">${Math.round(m.context_length / 1024)}k ctx</span>`
        : '';
      const finderBtn = m.path
        ? `<button class="btn btn-sm btn-ghost" title="Reveal in Finder" onclick="DexApp.revealModelPath('${escapeHtml(m.path)}')">${icon('folder', 13)}</button>`
        : '';
      const delBtn = (m.format === 'ollama' || !m.is_reference)
        ? `<button class="btn btn-sm btn-ghost" title="Delete model" onclick="DexApp.deleteModel('${escapeHtml(m.id)}')">${icon('trash', 13)}</button>`
        : '';

      return `
        <div class="library-row">
          <div class="library-row-main">
            <div class="library-row-head">
              <span class="library-model-name">${escapeHtml(m.name)}</span>
              ${fmtBadge}
              ${variantHtml}
              ${capsHtml}
              ${ctxBadge}
            </div>
            <div class="library-row-path" title="${escapeHtml(m.path || subText)}">${escapeHtml(subText)}</div>
          </div>
          <div class="library-row-actions">
            <span class="library-size">${formatSizeGb(m.size_bytes)}</span>
            <button class="btn btn-sm btn-primary" onclick="DexApp.useLibraryModel('${escapeHtml(m.id)}')">
              ${icon('zap', 12)} <span>Use Model</span>
            </button>
            ${finderBtn}
            ${delBtn}
          </div>
        </div>
      `;
    }).join('');
  }

  async function useLibraryModel(modelId) {
    const m = state.scannedModels.find((x) => x.id === modelId);
    if (!m) return;
    const noteEl = el('library-status-note');

    if (m.format === 'ollama') {
      el('sel-provider').value = 'ollama';
      el('input-model').value = m.name;
      el('input-apibase').value = '';
      syncModelLabels();
      navigate('view-agent');
      appendSystemNote(`Active model set to ${m.name} (Ollama)`);
      el('input-goal').focus();
      return;
    }

    if (m.format === 'mlx' && m.path) {
      if (noteEl) {
        noteEl.style.display = 'block';
        noteEl.textContent = `Activating MLX model '${m.name}' (checking LM Studio :1234 or starting mlx_lm.server)...`;
      }
      try {
        const res = await invoke('activate_mlx_model', {
          modelPath: m.path,
          modelName: m.name,
        });
        el('sel-provider').value = res.provider === 'lmstudio' ? 'lmstudio' : 'openai';
        el('input-model').value = res.model;
        el('input-apibase').value = res.api_base || '';
        syncModelLabels();
        if (noteEl) noteEl.style.display = 'none';
        navigate('view-agent');
        appendSystemNote(res.status_message);
        el('input-goal').focus();
      } catch (err) {
        if (noteEl) {
          noteEl.style.display = 'block';
          noteEl.textContent = String(err);
        }
      }
      return;
    }

    // GGUF / PT: prefer LM Studio :1234 if reachable and loaded, otherwise prefill composer
    const lm = state.localServers.find((s) => s.id === 'lmstudio' && s.reachable);
    if (lm && lm.loaded_models.length > 0) {
      const match = lm.loaded_models.find((id) => id.toLowerCase().includes(m.name.toLowerCase()));
      if (match) {
        el('sel-provider').value = 'lmstudio';
        el('input-model').value = match;
        el('input-apibase').value = lm.base_url;
        syncModelLabels();
        navigate('view-agent');
        appendSystemNote(`Connected to LM Studio serving '${match}'`);
        el('input-goal').focus();
        return;
      }
    }
    useModel(m.name, m.format);
  }

  async function revealModelPath(path) {
    try { await invoke('reveal_in_finder', { path }); } catch (err) { alert(err); }
  }

  function useModel(name, source) {
    el('input-model').value = name;
    if (source === 'ollama') {
      el('sel-provider').value = 'ollama';
      el('input-apibase').value = '';
    }
    syncModelLabels();
    navigate('view-agent');
    appendSystemNote(`Active model set to ${name} (${source})`);
    el('input-goal').focus();
  }

  async function deleteModel(modelId) {
    if (!confirm(`Delete model '${modelId}'?`)) return;
    try {
      await invoke('delete_model', { modelId });
      appendSystemNote(`Removed model ${modelId}`);
      refreshModelsLibrary();
    } catch (err) {
      alert(`Failed to delete model: ${err}`);
    }
  }

  // --- Explore Sub-View (2-Column Master-Detail Hugging Face Hub) ---
  async function searchHfModels() {
    const query = (el('hf-search-input') && el('hf-search-input').value.trim()) || '';
    const format = (el('hf-format-select') && el('hf-format-select').value) || 'all';
    const sort = (el('hf-sort-select') && el('hf-sort-select').value) || 'downloads';
    const container = el('hf-search-results');
    if (!container) return;
    container.innerHTML = '<div class="empty-note">Searching Hugging Face Hub...</div>';

    try {
      const repos = await invoke('search_hf_models', { query, format, sort, limit: 24 });
      state.hfReposCache = repos || [];
      if (state.hfReposCache.length === 0) {
        container.innerHTML = '<div class="empty-note">No models matched your query.</div>';
        return;
      }
      renderHfRepoList();
      // Auto-select the first result if nothing is selected or selection is no longer in list
      const stillPresent = state.hfReposCache.findIndex((r) => r.id === state.selectedHfRepoId);
      selectHfRepo(stillPresent >= 0 ? stillPresent : 0);
    } catch (err) {
      container.innerHTML = `<div class="empty-note" style="color: var(--err-fg);">Search failed: ${escapeHtml(err)}</div>`;
    }
  }

  function renderHfRepoList() {
    const container = el('hf-search-results');
    if (!container) return;
    container.innerHTML = state.hfReposCache.map((r, idx) => {
      const parts = r.id.split('/');
      const title = parts.length > 1 ? parts.slice(1).join('/') : r.id;
      const org = parts.length > 1 ? parts[0] : (r.author || '');
      const isAct = r.id === state.selectedHfRepoId;
      const fmts = (r.formats || []).map((f) => `<span class="badge ${formatBadgeClass(f)} mono" style="padding: 1px 5px; font-size: 9.5px;">${escapeHtml(f)}</span>`).join('');
      const caps = (r.capabilities || []).slice(0, 2).map((c) => {
        const ic = c === 'Vision' ? 'eye' : c === 'Reasoning' ? 'brain' : 'wrench';
        return `<span class="cap-chip" style="font-size: 9.5px;">${icon(ic, 10)}</span>`;
      }).join('');
      const relDate = formatRelativeDate(r.last_modified);
      return `
        <button class="repo-item ${isAct ? 'active' : ''}" onclick="DexApp.selectHfRepo(${idx})">
          <div class="repo-item-title" title="${escapeHtml(r.id)}">${escapeHtml(title)}</div>
          <div class="repo-item-org">${escapeHtml(org)}</div>
          <div class="row-wrap" style="gap: 4px;">${fmts}${caps}</div>
          <div class="repo-item-foot">
            <span>${(r.downloads || 0).toLocaleString()} dl · ${(r.likes || 0).toLocaleString()} likes</span>
            <span>${escapeHtml(relDate)}</span>
          </div>
        </button>
      `;
    }).join('');
  }

  async function selectHfRepo(idx) {
    const repo = state.hfReposCache[idx];
    if (!repo) return;
    state.selectedHfRepoId = repo.id;
    state.selectedHfFiles = [];
    renderHfRepoList();

    const detailEl = el('hf-detail-panel');
    if (!detailEl) return;
    const parts = repo.id.split('/');
    const title = parts.length > 1 ? parts.slice(1).join('/') : repo.id;
    const hfUrl = `https://huggingface.co/${repo.id}`;
    const fmts = (repo.formats || []).map((f) => `<span class="badge ${formatBadgeClass(f)} mono">${escapeHtml(f)}</span>`).join('');
    const caps = renderCapabilityChips(repo.capabilities || []);

    detailEl.innerHTML = `
      <div class="item-card-row" style="align-items: flex-start;">
        <div>
          <h2 style="font-size: 18px; font-weight: 600; letter-spacing: -0.02em;">${escapeHtml(title)}</h2>
          <div class="mono card-meta" style="margin-top: 2px;">${escapeHtml(repo.id)} · ${(repo.downloads || 0).toLocaleString()} downloads · ${(repo.likes || 0).toLocaleString()} likes</div>
        </div>
        <button class="btn btn-sm" onclick="DexApp.openExternalUrl('${escapeHtml(hfUrl)}')">
          ${icon('externalLink', 12)} <span>Open on Hugging Face</span>
        </button>
      </div>

      <div class="card" id="hf-download-card">
        <div class="card-header">
          <span class="card-title">${icon('download', 14)} Download Options</span>
          <span id="hf-variant-fit-badge" class="badge">Inspecting...</span>
        </div>
        <div id="hf-download-options-body">
          <div class="card-meta">Inspecting repository files and memory compatibility...</div>
        </div>
      </div>

      <div class="card">
        <div class="card-header">
          <span class="card-title">${icon('sliders', 14)} Details</span>
        </div>
        <div class="row-wrap" style="gap: 8px;">
          <span class="card-meta">Author: <strong style="color: var(--text-primary);">${escapeHtml(repo.author || parts[0] || '-')}</strong></span>
          ${fmts}
          ${caps}
          ${repo.last_modified ? `<span class="card-meta mono">Updated ${escapeHtml(formatRelativeDate(repo.last_modified))}</span>` : ''}
        </div>
      </div>

      <div class="card">
        <div class="card-header">
          <span class="card-title">${icon('fileText', 14)} README</span>
        </div>
        <div id="hf-readme-body" class="md-readme">
          <div class="card-meta">Loading README.md from Hugging Face...</div>
        </div>
      </div>
    `;

    // Fetch variants & README concurrently
    const currentRepoId = repo.id;
    const [filesRes, readmeRes] = await Promise.allSettled([
      invoke('list_hf_files', { repoId: currentRepoId }),
      invoke('get_hf_model_readme', { repoId: currentRepoId }),
    ]);

    if (state.selectedHfRepoId !== currentRepoId) return;

    const dlBody = el('hf-download-options-body');
    if (filesRes.status === 'fulfilled' && Array.isArray(filesRes.value) && filesRes.value.length > 0) {
      state.selectedHfFiles = filesRes.value;
      const defaultIdx = Math.max(0, filesRes.value.findIndex((f) => f.is_recommended));
      dlBody.innerHTML = `
        <div class="list-stack" style="gap: 10px;">
          <select id="hf-variant-select" class="select mono" onchange="DexApp.onSelectHfVariant()">
            ${filesRes.value.map((f, i) => {
              const recTag = f.is_recommended ? ' (Recommended)' : '';
              return `<option value="${i}" ${i === defaultIdx ? 'selected' : ''}>[${escapeHtml(f.format)} · ${escapeHtml(f.quantization)}] ${escapeHtml(f.filename)} — ${formatSizeGb(f.size_bytes)}${recTag}</option>`;
            }).join('')}
          </select>
          <div class="item-card-row">
            <span id="hf-variant-ram-note" class="card-meta mono"></span>
            <button id="btn-hf-download-selected" class="btn btn-primary" onclick="DexApp.downloadSelectedHfVariant()">
              ${icon('download', 13)} <span>Download</span>
            </button>
          </div>
        </div>
      `;
      onSelectHfVariant();
    } else {
      const msg = filesRes.status === 'rejected'
        ? `Could not inspect files: ${filesRes.reason}`
        : 'No downloadable GGUF or MLX weights found in this repository.';
      dlBody.innerHTML = `<div class="card-meta">${escapeHtml(msg)}</div>`;
      const badge = el('hf-variant-fit-badge');
      if (badge) { badge.className = 'badge'; badge.textContent = 'Unavailable'; }
    }

    const readmeBody = el('hf-readme-body');
    if (readmeBody) {
      if (readmeRes.status === 'fulfilled' && readmeRes.value) {
        readmeBody.innerHTML = renderSafeMarkdown(readmeRes.value);
      } else {
        readmeBody.innerHTML = '<div class="card-meta">No README.md available for this repository.</div>';
      }
    }
  }

  function onSelectHfVariant() {
    const sel = el('hf-variant-select');
    if (!sel) return;
    const f = state.selectedHfFiles[Number(sel.value) || 0];
    if (!f) return;

    const badge = el('hf-variant-fit-badge');
    if (badge) {
      if (f.memory_fit === 'fits') {
        badge.className = 'badge badge-ok';
        badge.textContent = 'Fits fully in Memory';
      } else if (f.memory_fit === 'tight') {
        badge.className = 'badge badge-warn';
        badge.textContent = 'Tight Fit';
      } else {
        badge.className = 'badge badge-err';
        badge.textContent = 'Too Large';
      }
    }
    const ramNote = el('hf-variant-ram-note');
    if (ramNote) {
      ramNote.textContent = `Estimated memory: ~${Number(f.ram_required_gb || 0).toFixed(1)} GB (${f.quantization})`;
    }
    const btn = el('btn-hf-download-selected');
    if (btn) {
      const isMlx = String(f.format || '').toUpperCase() === 'MLX';
      btn.innerHTML = `${icon('download', 13)} <span>${isMlx ? 'Open / Download' : 'Download'} (${formatSizeGb(f.size_bytes)})</span>`;
    }
  }

  async function downloadSelectedHfVariant() {
    const sel = el('hf-variant-select');
    if (!sel || !state.selectedHfRepoId) return;
    const f = state.selectedHfFiles[Number(sel.value) || 0];
    if (!f) return;
    if (String(f.format || '').toUpperCase() === 'MLX') {
      // Multi-shard MLX repos are best cloned/downloaded via huggingface-cli or LM Studio
      const hint = `hf download ${state.selectedHfRepoId} --local-dir ~/.dex/models/${state.selectedHfRepoId.split('/').pop()}`;
      if (confirm(`MLX repositories contain multiple safetensors shards.\n\nCopy / open repository on Hugging Face, or run:\n${hint}\n\nOpen repository page now?`)) {
        openExternalUrl(`https://huggingface.co/${state.selectedHfRepoId}`);
      }
      return;
    }
    await startDownload(state.selectedHfRepoId, f.filename, f.download_url);
  }

  async function inspectHfRepo(repoId) {
    const idx = state.hfReposCache.findIndex((r) => r.id === repoId);
    if (idx >= 0) await selectHfRepo(idx);
  }

  async function openExternalUrl(url) {
    try {
      await invoke('open_external_url', { url });
    } catch (err) {
      alert(`Could not open URL: ${err}`);
    }
  }

  // Safe Pure-JS Markdown Renderer (escapes all raw HTML before formatting)
  function renderSafeMarkdown(md) {
    const lines = String(md || '').replace(/\r\n/g, '\n').split('\n');
    let out = [];
    let inCode = false;
    let codeBuf = [];
    let inList = false;

    function formatInline(raw) {
      let s = escapeHtml(raw);
      s = s.replace(/`([^`]+)`/g, '<code>$1</code>');
      s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
      s = s.replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, (_, label, url) =>
        `<a href="#" onclick="event.preventDefault(); DexApp.openExternalUrl('${url}')">${label}</a>`
      );
      return s;
    }

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (line.trim().startsWith('```')) {
        if (inList) { out.push('</ul>'); inList = false; }
        if (!inCode) {
          inCode = true;
          codeBuf = [];
        } else {
          out.push(`<pre><code>${escapeHtml(codeBuf.join('\n'))}</code></pre>`);
          inCode = false;
        }
        continue;
      }
      if (inCode) {
        codeBuf.push(line);
        continue;
      }
      const trimmed = line.trim();
      if (!trimmed) {
        if (inList) { out.push('</ul>'); inList = false; }
        continue;
      }
      if (/^---+$/.test(trimmed)) {
        if (inList) { out.push('</ul>'); inList = false; }
        out.push('<hr />');
        continue;
      }
      const hMatch = trimmed.match(/^(#{1,4})\s+(.*)$/);
      if (hMatch) {
        if (inList) { out.push('</ul>'); inList = false; }
        const lvl = hMatch[1].length;
        out.push(`<h${lvl}>${formatInline(hMatch[2])}</h${lvl}>`);
        continue;
      }
      const liMatch = trimmed.match(/^[-*+]\s+(.*)$/);
      if (liMatch) {
        if (!inList) { out.push('<ul>'); inList = true; }
        out.push(`<li>${formatInline(liMatch[1])}</li>`);
        continue;
      }
      if (inList) { out.push('</ul>'); inList = false; }
      out.push(`<p>${formatInline(trimmed)}</p>`);
    }
    if (inCode) out.push(`<pre><code>${escapeHtml(codeBuf.join('\n'))}</code></pre>`);
    if (inList) out.push('</ul>');
    return out.join('\n');
  }

  async function startDownload(repoId, filename, downloadUrl) {
    try {
      await invoke('download_model', { repoId, filename, downloadUrl });
      el('download-progress-box').style.display = 'block';
      el('dl-title').textContent = `Downloading ${filename}`;
    } catch (err) {
      alert(`Download failed: ${err}`);
    }
  }

  async function cancelActiveDownload() {
    try {
      await invoke('cancel_download');
    } catch (err) {
      console.error('Cancel download error:', err);
    }
  }

  async function importLocalGguf() {
    const path = el('import-path').value.trim();
    const name = el('import-name').value.trim() || null;
    const toOllama = el('import-to-ollama').checked;
    if (!path) return alert('Specify a .gguf file or MLX model directory path.');
    try {
      const meta = await invoke('import_local_gguf', { path, name, toOllama });
      el('import-path').value = '';
      el('import-name').value = '';
      closeModelsDrawers();
      appendSystemNote(`Imported local model ${meta.name} (${meta.id})`);
      refreshModelsLibrary();
    } catch (err) {
      alert(`Import failed: ${err}`);
    }
  }

  async function pullOllamaModel() {
    const modelName = el('ollama-pull-input').value.trim();
    if (!modelName) return;
    try {
      await invoke('pull_ollama_model', { modelName });
      closeModelsDrawers();
      el('download-progress-box').style.display = 'block';
      el('dl-title').textContent = `Ollama: Pulling ${modelName}`;
      el('ollama-pull-input').value = '';
    } catch (err) {
      alert(`Ollama pull failed: ${err}`);
    }
  }

  // =========================================================================
  // Extensions Hub & Custom MCP Modal
  // =========================================================================
  function formatCategoryLabel(cat) {
    const map = {
      audio: 'Audio',
      image_video: 'Image & Video',
      three_d: '3D',
      ocr_docs: 'OCR & Docs',
      game_dev: 'GameDev',
      custom: 'Custom',
    };
    return map[cat] || cat;
  }

  function formatExtStatusBadge(status) {
    if (status === 'running') return '<span class="badge badge-ok">Running</span>';
    if (status === 'sleeping') return '<span class="badge badge-accent">Sleeping</span>';
    if (status === 'error') return '<span class="badge badge-err">Error</span>';
    return '<span class="badge">Stopped</span>';
  }

  function setExtCategory(cat, btnEl) {
    state.activeExtCategory = cat;
    document.querySelectorAll('#ext-filter-bar .filter-pill').forEach((b) => b.classList.remove('active'));
    if (btnEl) btnEl.classList.add('active');
    renderExtensions();
  }

  async function refreshExtensions() {
    try {
      const [catalog, installed] = await Promise.all([
        invoke('list_extension_catalog'),
        invoke('list_installed_extensions'),
      ]);
      state.cachedExtCatalog = catalog || [];
      state.cachedInstalledExts = installed || [];
      renderExtensions();
    } catch (err) {
      console.error('Refresh extensions failed:', err);
    }
  }

  function renderExtensions() {
    const installedEl = el('ext-installed-list');
    const catalogEl = el('ext-catalog-list');
    if (!installedEl || !catalogEl) return;

    const filteredInstalled = state.cachedInstalledExts.filter(
      (item) => state.activeExtCategory === 'all' || item.manifest.category === state.activeExtCategory
    );

    if (filteredInstalled.length === 0) {
      installedEl.innerHTML = '<div class="empty-note">No installed extensions in this category.</div>';
    } else {
      installedEl.innerHTML = filteredInstalled.map((item) => {
        const m = item.manifest;
        const isRunning = item.status === 'running';
        const pidNote = item.pid ? ` · PID ${item.pid}` : '';
        const timeoutOpts = [60, 300, 600, 1800, 0]
          .map((sec) => `<option value="${sec}" ${m.idle_timeout_secs === sec ? 'selected' : ''}>${sec === 0 ? 'No unload' : sec + 's idle'}</option>`)
          .join('');
        return `
          <div class="item-card">
            <div class="item-card-row">
              <div class="row-wrap" style="gap: 6px;">
                <span class="item-title">${escapeHtml(m.name)}</span>
                <span class="badge">${formatCategoryLabel(m.category)}</span>
                ${formatExtStatusBadge(item.status)}
              </div>
              <span class="card-meta mono">~${m.estimated_ram_mb} MB RAM${pidNote}</span>
            </div>
            <div class="item-desc">${escapeHtml(m.description)}</div>
            ${item.last_error ? `<div class="card-meta" style="color: var(--err-fg);">Error: ${escapeHtml(item.last_error)}</div>` : ''}
            <div class="item-card-row" style="padding-top: 4px;">
              <div class="row">
                <label class="checkbox-label">
                  <input type="checkbox" ${m.enabled ? 'checked' : ''} onchange="DexApp.toggleExt('${escapeHtml(m.id)}', this.checked)" />
                  Enabled
                </label>
                <select class="select mono" style="width: 112px; padding: 3px 6px; font-size: 11px;" onchange="DexApp.updateExtTimeout('${escapeHtml(m.id)}', Number(this.value))">
                  ${timeoutOpts}
                </select>
              </div>
              <div class="row">
                ${isRunning
                  ? `<button class="btn btn-sm btn-danger" onclick="DexApp.stopExt('${escapeHtml(m.id)}')">${icon('square', 10)} <span>Stop</span></button>`
                  : `<button class="btn btn-sm btn-primary" onclick="DexApp.startExt('${escapeHtml(m.id)}')">${icon('play', 11)} <span>Start</span></button>`}
                <button class="btn btn-sm btn-ghost" title="Remove extension" onclick="DexApp.removeExt('${escapeHtml(m.id)}')">${icon('trash', 12)}</button>
              </div>
            </div>
          </div>
        `;
      }).join('');
    }

    const installedIds = new Set(state.cachedInstalledExts.map((i) => i.manifest.id));
    const filteredCatalog = state.cachedExtCatalog.filter(
      (m) => state.activeExtCategory === 'all' || m.category === state.activeExtCategory
    );

    if (filteredCatalog.length === 0) {
      catalogEl.innerHTML = '<div class="empty-note">No built-in recipes in this category.</div>';
    } else {
      catalogEl.innerHTML = filteredCatalog.map((m) => {
        const isInstalled = installedIds.has(m.id);
        return `
          <div class="item-card">
            <div class="item-card-row">
              <div class="row-wrap" style="gap: 6px;">
                <span class="item-title">${escapeHtml(m.name)}</span>
                <span class="badge badge-accent">${formatCategoryLabel(m.category)}</span>
                <span class="card-meta mono">~${m.estimated_ram_mb} MB RAM</span>
              </div>
              ${isInstalled
                ? `<span class="badge badge-ok">${icon('check', 11)} Installed</span>`
                : `<button class="btn btn-sm btn-primary" onclick="DexApp.installCatalogExt('${escapeHtml(m.id)}')">${icon('plus', 12)} <span>Install</span></button>`}
            </div>
            <div class="item-desc">${escapeHtml(m.description)}</div>
            <div class="card-meta">${escapeHtml(m.install_hint)}</div>
          </div>
        `;
      }).join('');
    }
  }

  async function installCatalogExt(catalogId) {
    try {
      await invoke('install_catalog_extension', { catalogId });
      refreshExtensions();
    } catch (err) {
      alert(`Install failed: ${err}`);
    }
  }

  async function toggleExt(id, enabled) {
    try {
      await invoke('toggle_extension', { id, enabled });
      refreshExtensions();
    } catch (err) {
      alert(`Toggle failed: ${err}`);
    }
  }

  async function startExt(id) {
    try {
      await invoke('start_extension', { id });
      refreshExtensions();
    } catch (err) {
      alert(`Start failed: ${err}`);
      refreshExtensions();
    }
  }

  async function stopExt(id) {
    try {
      await invoke('stop_extension', { id });
      refreshExtensions();
    } catch (err) {
      alert(`Stop failed: ${err}`);
    }
  }

  async function removeExt(id) {
    if (!confirm(`Remove extension '${id}'?`)) return;
    try {
      await invoke('remove_extension', { id });
      refreshExtensions();
    } catch (err) {
      alert(`Remove failed: ${err}`);
    }
  }

  async function updateExtTimeout(id, idleTimeoutSecs) {
    try {
      await invoke('update_extension_timeout', { id, idleTimeoutSecs });
    } catch (err) {
      alert(`Update timeout failed: ${err}`);
    }
  }

  function openCustomMcpModal() {
    el('custom-mcp-modal').classList.add('open');
  }

  function closeCustomMcpModal() {
    el('custom-mcp-modal').classList.remove('open');
  }

  function toggleMcpTransportFields() {
    const transport = el('mcp-transport').value;
    el('mcp-stdio-fields').style.display = transport === 'stdio' ? 'grid' : 'none';
    el('mcp-sse-fields').style.display = transport === 'sse' ? 'flex' : 'none';
  }

  async function submitCustomMcp() {
    const id = el('mcp-id').value.trim();
    const name = el('mcp-name').value.trim();
    const transport = el('mcp-transport').value;
    const category = el('mcp-category').value;
    const command = el('mcp-command').value.trim() || null;
    const args = el('mcp-args').value.trim() || null;
    const url = el('mcp-url').value.trim() || null;
    const description = el('mcp-desc').value.trim() || null;
    const estimatedRamMb = Number(el('mcp-ram').value) || 512;
    const idleTimeoutSecs = Number(el('mcp-timeout').value);

    if (!id || !name) return alert('Extension ID and Display Name are required.');

    try {
      await invoke('add_custom_mcp_extension', {
        id,
        name,
        category,
        description,
        transport,
        command,
        args,
        url,
        estimatedRamMb,
        idleTimeoutSecs: Number.isNaN(idleTimeoutSecs) ? 600 : idleTimeoutSecs,
      });
      closeCustomMcpModal();
      ['mcp-id', 'mcp-name', 'mcp-command', 'mcp-args', 'mcp-url', 'mcp-desc'].forEach((fid) => {
        el(fid).value = '';
      });
      refreshExtensions();
    } catch (err) {
      alert(`Failed to add custom MCP: ${err}`);
    }
  }

  // =========================================================================
  // Audio, Voice Push-to-Talk, OCR & Document Reader
  // =========================================================================
  async function toggleVoiceRecording() {
    const micBtn = el('btn-mic');
    const meter = el('voice-status-bar');

    if (!state.isRecordingVoice) {
      try {
        await invoke('start_voice_recording');
        state.isRecordingVoice = true;
        micBtn.classList.add('recording');
        micBtn.innerHTML = icon('square', 11);
        meter.classList.add('active');
      } catch (err) {
        alert(`Microphone capture failed: ${err}`);
      }
    } else {
      state.isRecordingVoice = false;
      micBtn.classList.remove('recording');
      micBtn.innerHTML = icon('clock', 14);
      meter.classList.remove('active');
      try {
        const res = await invoke('stop_and_transcribe');
        micBtn.innerHTML = icon('mic', 14);
        refreshAudioMedia();
        if (res.transcript) {
          const ta = el('input-goal');
          const cur = ta.value.trim();
          ta.value = cur ? `${cur} ${res.transcript}` : res.transcript;
          autoResizeComposer();
          ta.focus();
        } else {
          appendSystemNote(`Voice saved to ${res.audio_path} (${res.duration_secs}s). ${res.stt_warning || ''}`);
        }
      } catch (err) {
        micBtn.innerHTML = icon('mic', 14);
        alert(`Transcription failed: ${err}`);
      }
    }
  }

  async function speakTextFromUi() {
    const text = el('tts-text-input').value.trim();
    const voice = el('tts-voice-input').value.trim() || null;
    if (!text) return;
    try {
      await invoke('speak_text', { text, voice });
      el('tts-text-input').value = '';
      refreshAudioMedia();
    } catch (err) {
      alert(`TTS failed: ${err}`);
    }
  }

  async function refreshAudioMedia() {
    const container = el('audio-gallery-list');
    if (!container) return;
    try {
      const items = await invoke('list_audio_media');
      if (!items || items.length === 0) {
        container.innerHTML = '<div class="empty-note" style="grid-column: 1 / -1;">No audio files recorded or synthesized yet.</div>';
        return;
      }
      container.innerHTML = items.map((item) => `
        <div class="audio-card">
          <div class="item-card-row">
            <div class="row" style="min-width: 0;">
              <span class="badge badge-accent">${escapeHtml(item.kind)}</span>
              <span class="mono" style="font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${escapeHtml(item.filename)}</span>
            </div>
            <span class="card-meta mono">${formatBytesKb(item.size_bytes)}</span>
          </div>
          <audio controls preload="metadata" src="${item.data_url}"></audio>
        </div>
      `).join('');
    } catch (err) {
      container.innerHTML = `<div class="empty-note">${escapeHtml(err)}</div>`;
    }
  }

  async function runOcrFromUi() {
    const path = el('doc-path-input').value.trim();
    if (!path) return alert('Enter an image path (.png, .jpg, .webp).');
    const metaEl = el('doc-result-meta');
    const boxEl = el('doc-result-box');
    const sendBtn = el('btn-send-doc-agent');
    metaEl.textContent = 'Running OCR...';
    boxEl.style.display = 'none';
    sendBtn.style.display = 'none';

    try {
      const res = await invoke('run_ocr_image', { path });
      metaEl.textContent = `Engine: ${res.engine} · Blocks: ${res.blocks.length}`;
      boxEl.textContent = res.text || '(No text detected)';
      boxEl.style.display = 'block';
      sendBtn.style.display = 'inline-flex';
      state.lastParsedDoc = { path, engine: res.engine, text: res.text || '' };
    } catch (err) {
      metaEl.textContent = `OCR failed: ${err}`;
    }
  }

  async function parseDocFromUi() {
    const path = el('doc-path-input').value.trim();
    if (!path) return alert('Enter a document path (.pdf, .docx, .md, .png).');
    const metaEl = el('doc-result-meta');
    const boxEl = el('doc-result-box');
    const sendBtn = el('btn-send-doc-agent');
    metaEl.textContent = 'Parsing document...';
    boxEl.style.display = 'none';
    sendBtn.style.display = 'none';

    try {
      const res = await invoke('parse_document', { path });
      const pages = res.page_count ? ` · Pages: ${res.page_count}` : '';
      metaEl.textContent = `Engine: ${res.engine}${pages}`;
      boxEl.textContent = res.markdown;
      boxEl.style.display = 'block';
      sendBtn.style.display = 'inline-flex';
      state.lastParsedDoc = {
        path: res.saved_full_path || res.source_path,
        engine: res.engine,
        text: res.markdown,
      };
    } catch (err) {
      metaEl.textContent = `Parse failed: ${err}`;
    }
  }

  function sendDocToAgent() {
    if (!state.lastParsedDoc) return;
    const d = state.lastParsedDoc;
    const snippet = d.text.slice(0, 600);
    const prompt = `Analyze the extracted document (${d.path}, engine: ${d.engine}):\n\n${snippet}${d.text.length > 600 ? '\n...[use read_file or doc_parse for full content]' : ''}`;
    sendPromptToAgent(prompt, `Loaded extracted document '${d.path}' into composer`);
  }

  // =========================================================================
  // Media Studio (Image Gen, Upscale, Process, Video Inspect, Visual Gallery)
  // =========================================================================
  async function refreshComfyWorkflows() {
    const sel = el('media-workflow-select');
    if (!sel) return;
    try {
      const workflows = await invoke('list_comfyui_workflows');
      if (workflows && workflows.length > 0) {
        const prev = sel.value;
        sel.innerHTML = workflows
          .map((w) => `<option value="${escapeHtml(w)}">${escapeHtml(w)}</option>`)
          .join('');
        if (workflows.includes(prev)) sel.value = prev;
      }
    } catch (err) {
      console.error('Workflows error:', err);
    }
  }

  async function generateImageFromUi() {
    const prompt = el('media-prompt-input').value.trim();
    if (!prompt) return alert('Enter a prompt to generate an image.');
    const negativePrompt = el('media-neg-input').value.trim() || null;
    const [wStr, hStr] = (el('media-res-select').value || '1024x1024').split('x');
    const width = parseInt(wStr, 10) || 1024;
    const height = parseInt(hStr, 10) || 1024;
    const steps = parseInt(el('media-steps-input').value, 10) || 20;
    const backend = el('media-backend-select').value || 'auto';
    const workflow = el('media-workflow-select').value || null;

    const btn = el('btn-generate-image');
    const statusEl = el('media-gen-status');
    btn.disabled = true;
    statusEl.textContent = `Generating ${width}x${height} (${steps} steps, ${backend})...`;

    try {
      const res = await invoke('generate_image', {
        prompt,
        negativePrompt,
        width,
        height,
        steps,
        backend,
        workflow,
        model: null,
      });
      statusEl.textContent = `Saved ${res.width}x${res.height} via ${res.backend} (seed ${res.seed}): ${res.path}`;
      refreshVisualMedia();
    } catch (err) {
      statusEl.textContent = `Error: ${err}`;
    } finally {
      btn.disabled = false;
    }
  }

  async function refreshVisualMedia() {
    const container = el('visual-media-gallery');
    if (!container) return;
    try {
      const items = await invoke('list_visual_media');
      state.visualMediaCache = items || [];
      if (state.visualMediaCache.length === 0) {
        container.innerHTML = '<div class="empty-note" style="grid-column: 1 / -1;">No images or videos in .dex/media/{images,video}/.</div>';
        return;
      }
      container.innerHTML = state.visualMediaCache.map((item, idx) => {
        let previewHtml;
        if (item.data_url && item.kind === 'video') {
          previewHtml = `<video controls muted playsinline src="${item.data_url}"></video>`;
        } else if (item.data_url) {
          previewHtml = `<img src="${item.data_url}" alt="${escapeHtml(item.filename)}" style="cursor: pointer;" onclick="window.open('${item.data_url}')" />`;
        } else {
          previewHtml = `<div class="media-card-placeholder">${icon(item.kind === 'video' ? 'film' : 'image', 20)}<span>${escapeHtml(item.format.toUpperCase())} (${formatBytesKb(item.size_bytes)})</span></div>`;
        }
        const upscaleBtn = item.kind === 'image'
          ? `<button class="btn btn-sm" onclick="DexApp.upscaleGalleryItem(${idx})">${icon('maximize', 12)} <span>2x</span></button>`
          : '';
        return `
          <div class="media-card">
            ${previewHtml}
            <div class="media-card-body">
              <div class="media-card-title" title="${escapeHtml(item.path)}">${escapeHtml(item.filename)}</div>
              <div class="media-card-meta">
                <span>${escapeHtml(item.format.toUpperCase())}</span>
                <span>${formatBytesKb(item.size_bytes)}</span>
              </div>
              <div class="row-wrap" style="gap: 4px;">
                ${upscaleBtn}
                <button class="btn btn-sm" onclick="DexApp.sendVisualMediaToAgent(${idx})">${icon('arrowUpRight', 12)} <span>Agent</span></button>
                <button class="btn btn-sm btn-ghost" title="Reveal in Finder" onclick="DexApp.revealGalleryItem(${idx})">${icon('folder', 12)}</button>
              </div>
            </div>
          </div>
        `;
      }).join('');
    } catch (err) {
      container.innerHTML = `<div class="empty-note">${escapeHtml(err)}</div>`;
    }
  }

  async function upscaleGalleryItem(idx) {
    const item = state.visualMediaCache[idx];
    if (!item) return;
    const statusEl = el('media-gen-status');
    if (statusEl) statusEl.textContent = `Upscaling ${item.filename} 2x...`;
    try {
      const res = await invoke('upscale_image', { path: item.path, scale: 2, backend: 'auto' });
      if (statusEl) statusEl.textContent = `Upscaled ${res.width}x${res.height} via ${res.backend}: ${res.output_path}`;
      refreshVisualMedia();
    } catch (err) {
      if (statusEl) statusEl.textContent = `Upscale error: ${err}`;
    }
  }

  function sendVisualMediaToAgent(idx) {
    const item = state.visualMediaCache[idx];
    if (!item) return;
    sendPromptToAgent(
      `Inspect and use the visual asset at '${item.path}' (${item.kind}, ${item.format}).`,
      `Attached visual asset '${item.filename}' to composer`
    );
  }

  async function revealGalleryItem(idx) {
    const item = state.visualMediaCache[idx];
    if (!item) return;
    try { await invoke('reveal_in_finder', { path: item.path }); } catch (err) { alert(err); }
  }

  async function upscaleAssetPathFromUi() {
    const path = el('media-asset-path').value.trim();
    if (!path) return alert('Enter an image path first.');
    const out = el('media-utility-output');
    out.style.display = 'block';
    out.textContent = `Upscaling ${path} 2x...`;
    try {
      const res = await invoke('upscale_image', { path, scale: 2, backend: 'auto' });
      out.textContent = JSON.stringify(res, null, 2);
      refreshVisualMedia();
    } catch (err) {
      out.textContent = `Error: ${err}`;
    }
  }

  async function convertAssetWebpFromUi() {
    const path = el('media-asset-path').value.trim();
    if (!path) return alert('Enter an image path first.');
    const out = el('media-utility-output');
    out.style.display = 'block';
    out.textContent = `Converting ${path} to WebP...`;
    try {
      const res = await invoke('process_image', {
        path,
        resizeWidth: null,
        resizeHeight: null,
        format: 'webp',
        outputPath: null,
      });
      out.textContent = JSON.stringify(res, null, 2);
      refreshVisualMedia();
    } catch (err) {
      out.textContent = `Error: ${err}`;
    }
  }

  async function inspectVideoFromUi() {
    const path = el('media-asset-path').value.trim();
    if (!path) return alert('Enter a video path first.');
    const out = el('media-utility-output');
    out.style.display = 'block';
    out.textContent = `Inspecting ${path} via ffprobe...`;
    try {
      const res = await invoke('inspect_video_file', { path });
      out.textContent = JSON.stringify(res, null, 2);
    } catch (err) {
      out.textContent = `Error: ${err}`;
    }
  }

  // =========================================================================
  // 3D & Game Studio (Uses window.DexViewer3D + All 10 Phase 9 IPC Commands)
  // =========================================================================
  function ensure3dViewer() {
    if (!state.glbViewer && global.DexViewer3D) {
      const canvas = el('glb-canvas');
      if (canvas) state.glbViewer = new global.DexViewer3D.DexGlbViewer(canvas);
    } else if (state.glbViewer) {
      state.glbViewer.resize();
      state.glbViewer.render();
    }
    return state.glbViewer;
  }

  async function init3dStudioView() {
    ensure3dViewer();
    refreshBlenderStatus();
    refresh3dSourceImages();
    await refresh3dModels();
  }

  function toggle3dWireframe() {
    const v = ensure3dViewer();
    if (!v) return;
    v.wireframe = !v.wireframe;
    el('btn-3d-wireframe').innerHTML = `${icon('wireframe', 13)} <span>Wireframe: ${v.wireframe ? 'On' : 'Off'}</span>`;
    v.render();
  }

  function toggle3dGrid() {
    const v = ensure3dViewer();
    if (!v) return;
    v.showGrid = !v.showGrid;
    el('btn-3d-grid').innerHTML = `${icon('grid', 13)} <span>Grid: ${v.showGrid ? 'On' : 'Off'}</span>`;
    v.render();
  }

  function reset3dCamera() {
    const v = ensure3dViewer();
    if (v) v.resetCamera();
  }

  function update3dHud(report, extraNote) {
    const hud = el('viewer-3d-hud');
    const warnBox = el('viewer-3d-warnings');
    if (!hud) return;

    if (!report) {
      hud.innerHTML = `
        <div style="font-weight: 600;">Offline WebGL2 GLB Viewer</div>
        <div style="color: var(--text-secondary);">${escapeHtml(extraNote || 'Select a .glb model or generate a sample cube.')}</div>
      `;
      if (warnBox) warnBox.style.display = 'none';
      return;
    }

    const dims = report.bounding_box
      ? `${report.bounding_box.dimensions.map((d) => d.toFixed(2)).join(' x ')} u`
      : 'n/a';
    const warnCount = (report.game_ready_warnings || []).length;
    const statusHtml = warnCount === 0
      ? '<span class="badge badge-ok">Game-Ready</span>'
      : `<span class="badge badge-warn">${warnCount} Warning(s)</span>`;
    const name = report.path.split('/').pop();

    hud.innerHTML = `
      <div class="row" style="gap: 8px; margin-bottom: 3px;">
        <span class="mono" style="font-weight: 600;">${escapeHtml(name)}</span>
        ${statusHtml}
      </div>
      <div class="mono" style="font-size: 11px; color: var(--text-secondary);">
        Verts: ${report.vertex_count.toLocaleString()} · Tris: ${report.triangle_count.toLocaleString()} · Dims: ${dims}
      </div>
      <div class="mono" style="font-size: 10.5px; color: var(--text-muted);">
        Normals: ${report.has_normals ? 'Yes' : 'No'} · UVs: ${report.has_uvs ? 'Yes' : 'No'} · Mats: ${escapeHtml((report.material_names || []).join(', ') || 'none')}
      </div>
      ${extraNote ? `<div style="color: var(--warn-fg); margin-top: 3px;">${escapeHtml(extraNote)}</div>` : ''}
    `;

    if (warnBox) {
      if (warnCount > 0) {
        warnBox.style.display = 'block';
        warnBox.innerHTML = report.game_ready_warnings
          .map((w) => `<div class="row" style="gap: 6px;">${icon('alertTriangle', 13)} <span>${escapeHtml(w)}</span></div>`)
          .join('');
      } else {
        warnBox.style.display = 'none';
      }
    }
  }

  async function load3dModelIntoViewer(path) {
    state.selected3dModel = path;
    const label = el('selected-3d-label');
    if (label) label.textContent = path.split('/').pop();
    render3dGallerySelection();

    let report = null;
    try {
      report = await invoke('inspect_3d_model', { path });
      state.selected3dReport = report;
    } catch (err) {
      state.selected3dReport = null;
      update3dHud(null, `Inspect note: ${err}`);
    }

    if (path.toLowerCase().endsWith('.glb')) {
      try {
        const b64 = await invoke('read_glb_base64', { path });
        const bytes = global.DexViewer3D.base64ToUint8Array(b64);
        const v = ensure3dViewer();
        if (v) v.loadGlbBytes(bytes);
        update3dHud(report, null);
      } catch (err) {
        update3dHud(report, `Viewer: ${err}`);
      }
    } else {
      update3dHud(report, 'Interactive WebGL2 preview requires .glb (convert via Blender)');
    }
  }

  async function createSample3dCubeFromUi() {
    const statusEl = el('gen3d-status');
    if (statusEl) statusEl.textContent = 'Generating glTF 2.0 sample cube...';
    try {
      const report = await invoke('create_sample_3d_model');
      if (statusEl) statusEl.textContent = `Sample cube created: ${report.path}`;
      await refresh3dModels();
      await load3dModelIntoViewer(report.path);
    } catch (err) {
      if (statusEl) statusEl.textContent = `Error: ${err}`;
    }
  }

  async function refreshBlenderStatus() {
    const badge = el('blender-status-badge');
    if (!badge) return;
    try {
      const st = await invoke('check_blender_status');
      if (st && st.available) {
        badge.className = 'badge badge-ok';
        badge.textContent = st.version || 'Blender Ready';
      } else {
        badge.className = 'badge';
        badge.textContent = 'Blender: Not Found';
      }
    } catch (_) {
      badge.className = 'badge';
      badge.textContent = 'Blender: Unavailable';
    }
  }

  async function refresh3dSourceImages() {
    const sel = el('gen3d-image-select');
    if (!sel) return;
    try {
      const items = await invoke('list_visual_media');
      const images = (items || []).filter((i) => i.kind === 'image');
      sel.innerHTML = '<option value="">Select generated image...</option>' +
        images.map((i) => `<option value="${escapeHtml(i.path)}">${escapeHtml(i.filename)}</option>`).join('');
    } catch (_) {}
  }

  function onSelect3dSourceImage() {
    const val = el('gen3d-image-select').value;
    if (val) el('gen3d-image-path').value = val;
  }

  async function generate3dFromUi() {
    const imagePath = el('gen3d-image-path').value.trim() || null;
    const prompt = el('gen3d-prompt').value.trim() || null;
    if (!imagePath && !prompt) return alert('Select an input image or enter a prompt.');

    const backend = el('gen3d-backend').value || 'auto';
    const textureResolution = parseInt(el('gen3d-texres').value, 10) || 1024;
    const removeBackground = el('gen3d-rmbg').checked;

    const btn = el('btn-gen3d');
    const statusEl = el('gen3d-status');
    btn.disabled = true;
    statusEl.textContent = `Generating 3D mesh via ${backend}...`;

    try {
      const res = await invoke('generate_3d_model', {
        imagePath,
        prompt,
        backend,
        textureResolution,
        removeBackground,
      });
      statusEl.textContent = `Generated ${res.path} (${res.report.triangle_count} tris)`;
      await refresh3dModels();
      await load3dModelIntoViewer(res.path);
    } catch (err) {
      statusEl.textContent = `Error: ${err}`;
    } finally {
      btn.disabled = false;
    }
  }

  async function refresh3dModels() {
    const container = el('models-3d-gallery');
    if (!container) return;
    try {
      const items = await invoke('list_3d_models');
      state.models3dCache = items || [];
      if (state.models3dCache.length === 0) {
        container.innerHTML = '<div class="empty-note" style="grid-column: 1 / -1;">No 3D models stored in .dex/media/3d/. Click "Generate Sample Cube" above.</div>';
        return;
      }
      render3dGallerySelection();
    } catch (err) {
      container.innerHTML = `<div class="empty-note">${escapeHtml(err)}</div>`;
    }
  }

  function render3dGallerySelection() {
    const container = el('models-3d-gallery');
    if (!container || state.models3dCache.length === 0) return;
    container.innerHTML = state.models3dCache.map((m, idx) => {
      const isSel = state.selected3dModel === m.path;
      const tris = m.triangle_count !== null && m.triangle_count !== undefined
        ? `${m.triangle_count.toLocaleString()} tris`
        : m.format.toUpperCase();
      return `
        <div class="model3d-card ${isSel ? 'selected' : ''}" onclick="DexApp.select3dGalleryItem(${idx})">
          <div style="min-width: 0;">
            <div class="row" style="gap: 6px;">
              ${icon('cube', 13)}
              <span class="mono" style="font-size: 12px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${escapeHtml(m.filename)}</span>
            </div>
            <div class="card-meta mono" style="margin-top: 3px;">
              ${escapeHtml(m.format.toUpperCase())} · ${formatBytesKb(m.size_bytes)} · ${tris}
            </div>
          </div>
          <div class="row" style="flex-shrink: 0;">
            <button class="btn btn-sm btn-ghost" title="Reveal in Finder" onclick="event.stopPropagation(); DexApp.reveal3dGalleryItem(${idx})">${icon('folder', 12)}</button>
          </div>
        </div>
      `;
    }).join('');
  }

  function select3dGalleryItem(idx) {
    const item = state.models3dCache[idx];
    if (item) load3dModelIntoViewer(item.path);
  }

  async function reveal3dGalleryItem(idx) {
    const item = state.models3dCache[idx];
    if (!item) return;
    try { await invoke('reveal_in_finder', { path: item.path }); } catch (err) { alert(err); }
  }

  function show3dActionOutput(text) {
    const box = el('studio3d-action-output');
    if (box) {
      box.style.display = 'block';
      box.textContent = text;
    }
  }

  async function convertSelected3dWithBlender() {
    if (!state.selected3dModel) return alert('Select a 3D model first.');
    const targetFmt = el('blender-target-fmt').value || 'fbx';
    const ratioVal = parseFloat(el('blender-decimate-ratio').value);
    const decimateRatio = (!Number.isNaN(ratioVal) && ratioVal < 1.0) ? ratioVal : null;
    const stem = state.selected3dModel.replace(/\.[^/.]+$/, '');
    const suffix = decimateRatio ? `_lod${Math.round(decimateRatio * 100)}` : '_conv';
    const outputPath = `${stem}${suffix}.${targetFmt}`;

    show3dActionOutput(`Converting via headless Blender: ${state.selected3dModel} -> ${outputPath}...`);
    try {
      const res = await invoke('convert_3d_model', {
        inputPath: state.selected3dModel,
        outputPath,
        decimateRatio,
        applyTransforms: true,
      });
      show3dActionOutput(JSON.stringify(res, null, 2));
      refresh3dModels();
    } catch (err) {
      show3dActionOutput(`Error: ${err}`);
    }
  }

  async function renderSelected3dPreview() {
    if (!state.selected3dModel) return alert('Select a 3D model first.');
    show3dActionOutput(`Rendering studio preview of ${state.selected3dModel}...`);
    try {
      const res = await invoke('render_3d_preview', {
        inputPath: state.selected3dModel,
        outputPng: null,
        width: 768,
        height: 768,
      });
      show3dActionOutput(`Preview rendered (${res.width}x${res.height}): ${res.preview_png}`);
    } catch (err) {
      show3dActionOutput(`Error: ${err}`);
    }
  }

  function sendSelected3dToAgent() {
    if (!state.selected3dModel) return alert('Select a 3D model first.');
    const tris = state.selected3dReport
      ? ` (${state.selected3dReport.triangle_count} triangles, ${state.selected3dReport.vertex_count} vertices)`
      : '';
    sendPromptToAgent(
      `Inspect and work with the 3D model at '${state.selected3dModel}'${tris}.`,
      `Attached 3D model '${state.selected3dModel}' to composer`
    );
  }

  async function revealSelected3dInFinder() {
    if (!state.selected3dModel) return alert('Select a 3D model first.');
    try { await invoke('reveal_in_finder', { path: state.selected3dModel }); } catch (err) { alert(err); }
  }

  async function inspectUnityProjectFromUi() {
    const projectDir = el('unity-project-path').value.trim();
    if (!projectDir) return alert('Enter a Unity project directory path first.');
    show3dActionOutput(`Inspecting Unity project '${projectDir}'...`);
    try {
      const report = await invoke('inspect_unity_project', { projectDir });
      show3dActionOutput(JSON.stringify(report, null, 2));
    } catch (err) {
      show3dActionOutput(`Error: ${err}`);
    }
  }

  async function exportSelected3dToUnity() {
    if (!state.selected3dModel) return alert('Select a 3D model first.');
    const projectDir = el('unity-project-path').value.trim();
    if (!projectDir) return alert('Enter a Unity project directory path first.');
    const subdir = el('unity-subdir-input').value.trim() || 'Assets/DexGenerated';
    const overwrite = el('unity-overwrite').checked;

    show3dActionOutput(`Exporting '${state.selected3dModel}' to '${projectDir}/${subdir}'...`);
    try {
      const res = await invoke('export_model_to_unity', {
        meshPath: state.selected3dModel,
        projectDir,
        subdir,
        overwrite,
      });
      show3dActionOutput(JSON.stringify(res, null, 2));
    } catch (err) {
      show3dActionOutput(`Error: ${err}`);
    }
  }

  // =========================================================================
  // Tauri Event Subscriptions (100% Parity with Rust Backend)
  // =========================================================================
  function registerEventListeners() {
    if (!global.__TAURI__) return;

    listen('dex-event', (event) => {
      const ev = event.payload;
      if (!ev || !ev.type) return;
      const d = ev.data || {};

      if (ev.type === 'ThoughtToken') {
        appendThoughtToken(d.token || '');
      } else if (ev.type === 'ToolStarted') {
        appendToolStartedCard(d.tool_name, d.arguments);
      } else if (ev.type === 'ToolFinished') {
        resolveToolFinishedCard(d.tool_name, d.output, !!d.is_error);
        if (state.inspectorOpen && state.inspectorTab === 'tasks') refreshTasks();
        if (d.tool_name && d.tool_name.includes('screenshot')) refreshScreenshots();
      } else if (ev.type === 'SubagentSpawned') {
        recordSubagentSpawn(d.role, d.task);
        appendSystemNote(`Subagent spawned: ${d.role} — ${d.task}`);
      } else if (ev.type === 'SubagentCompleted') {
        recordSubagentComplete(d.role);
        appendSystemNote(`Subagent completed: ${d.role}`);
      } else if (ev.type === 'TaskUpdated') {
        if (state.inspectorOpen) refreshTasks();
      } else if (ev.type === 'ContextCompacted') {
        updateContextMetric(d.estimated_tokens || 0);
        appendSystemNote(`Context compacted to ~${d.estimated_tokens} tokens`);
      } else if (ev.type === 'SecurityBlocked') {
        appendSystemNote(`Security hook halted ${d.tool_name}: ${d.reason}`, 'warn');
      } else if (ev.type === 'ModelDownloadProgress') {
        const box = el('download-progress-box');
        if (box) {
          box.style.display = 'block';
          el('dl-title').textContent = `Downloading ${d.file_name}`;
          el('dl-speed').textContent = `${Number(d.speed_mbps || 0).toFixed(1)} MB/s`;
          const pct = d.total_bytes > 0 ? (d.downloaded_bytes / d.total_bytes) * 100 : 0;
          el('dl-bar').style.width = `${pct}%`;
          const dlMb = (d.downloaded_bytes / (1024 * 1024)).toFixed(1);
          const totMb = (d.total_bytes / (1024 * 1024)).toFixed(1);
          el('dl-stats').textContent = `${dlMb} MB / ${totMb} MB (${pct.toFixed(1)}%)`;
        }
      } else if (ev.type === 'ModelDownloadCompleted') {
        el('dl-stats').textContent = 'Complete. Saved to local registry.';
        el('dl-bar').style.width = '100%';
        refreshInstalledModels();
        setTimeout(() => { el('download-progress-box').style.display = 'none'; }, 3000);
      } else if (ev.type === 'ModelDownloadFailed') {
        el('dl-stats').textContent = `Failed: ${d.error}`;
        setTimeout(() => { el('download-progress-box').style.display = 'none'; }, 4000);
      } else if (ev.type === 'ExtensionStatusChanged') {
        if (state.activeView === 'view-extensions') refreshExtensions();
      } else if (ev.type === 'ToolsActivated') {
        appendSystemNote(`Activated deferred tools (${d.source}): ${(d.tool_names || []).join(', ')}`);
      } else if (ev.type === 'MediaGenerationProgress') {
        const total = Math.max(1, d.total_steps || 1);
        const pct = Math.min(100, Math.round(((d.step || 0) / total) * 100));
        const is3d = d.kind === '3d';
        const box = el(is3d ? 'gen3d-progress-box' : 'media-progress-box');
        if (box) {
          box.style.display = 'block';
          el(is3d ? 'gen3d-progress-label' : 'media-progress-label').textContent = d.message || 'Processing...';
          el(is3d ? 'gen3d-progress-steps' : 'media-progress-steps').textContent = `${d.step} / ${total} (${pct}%)`;
          el(is3d ? 'gen3d-progress-bar' : 'media-progress-bar').style.width = `${pct}%`;
          if (d.step >= total) {
            setTimeout(() => { box.style.display = 'none'; }, 2400);
          }
        }
      } else if (ev.type === 'MediaGenerated') {
        if (['tts', 'music', 'sfx', 'recording'].includes(d.kind)) refreshAudioMedia();
        if (['image', 'video'].includes(d.kind)) {
          refreshVisualMedia();
          refresh3dSourceImages();
        }
        if (d.kind === '3d') refresh3dModels();
      } else if (ev.type === 'VoiceCaptureState') {
        const badge = el('voice-db-badge');
        if (badge) badge.textContent = `${Number(d.level_db || -60).toFixed(1)} dB`;
      }
    });

    listen('dex-approval-request', (event) => {
      const data = event.payload || {};
      showApprovalModal(data.tool, data.arguments);
    });

    listen('dex-goal-finished', (event) => {
      const data = event.payload || {};
      setRunningState(false);
      if (data.status === 'success') {
        appendFinalAnswer(data.answer || 'Objective completed.');
      } else if (data.status === 'cancelled') {
        appendSystemNote(data.error || 'Execution cancelled by user.', 'warn');
      } else {
        appendSystemNote(data.error || 'Execution failed.', 'error');
      }
    });

    listen('dex-mode-changed', (event) => {
      if (event.payload && event.payload.mode) {
        updateModeButtons(event.payload.mode);
      }
    });

    listen('dex-open-tab', (event) => {
      const tab = event.payload && event.payload.tab;
      if (tab === 'cookbook') navigate('view-cookbook');
    });
  }

  // =========================================================================
  // Bootstrap
  // =========================================================================
  function bindGlobalUiEvents() {
    const ta = el('input-goal');
    if (ta) {
      ta.addEventListener('input', autoResizeComposer);
      ta.addEventListener('keydown', (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
          e.preventDefault();
          runGoal();
        } else if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault();
          runGoal();
        }
      });
    }

    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        closeModelPopover();
        closeCustomMcpModal();
      } else if ((e.metaKey || e.ctrlKey) && e.key === '\\') {
        e.preventDefault();
        toggleSidebar();
      } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'i') {
        e.preventDefault();
        toggleInspector();
      }
    });

    document.addEventListener('click', (e) => {
      const pop = el('model-popover');
      const pill = el('btn-model-pill');
      if (pop && pop.classList.contains('open')) {
        if (!pop.contains(e.target) && pill && !pill.contains(e.target)) {
          closeModelPopover();
        }
      }
    });
  }

  async function bootstrap() {
    global.DexIcons.hydrate(document);
    bindGlobalUiEvents();
    syncModelLabels();
    registerEventListeners();

    // Sync initial execution mode & host hardware summary quietly
    if (global.__TAURI__) {
      try {
        const mode = await invoke('get_mode');
        if (mode) updateModeButtons(mode);
      } catch (_) {}
      refreshCookbook();
      refreshTasks();
    }
  }

  global.DexApp = {
    navigate,
    toggleSidebar,
    toggleInspector,
    switchInspector,
    toggleModelPopover,
    closeModelPopover,
    openModelConfig,
    syncModelLabels,
    onProviderChange,
    useQuickPrompt,
    clearAgentFeed,
    setMode,
    runGoal,
    stopGoal,
    resolveApproval,
    refreshTasks,
    refreshScreenshots,
    refreshCookbook,
    refreshInstalledModels,
    initModelsHubView,
    switchModelsTab,
    toggleModelsDrawer,
    closeModelsDrawers,
    refreshModelsLibrary,
    connectLocalServer,
    addModelDirFromUi,
    removeModelDirFromUi,
    setLibraryFormat,
    selectLibraryVariant,
    renderLibraryModels,
    useLibraryModel,
    revealModelPath,
    useModel,
    deleteModel,
    searchHfModels,
    selectHfRepo,
    onSelectHfVariant,
    downloadSelectedHfVariant,
    inspectHfRepo,
    openExternalUrl,
    startDownload,
    cancelActiveDownload,
    importLocalGguf,
    pullOllamaModel,
    setExtCategory,
    refreshExtensions,
    installCatalogExt,
    toggleExt,
    startExt,
    stopExt,
    removeExt,
    updateExtTimeout,
    openCustomMcpModal,
    closeCustomMcpModal,
    toggleMcpTransportFields,
    submitCustomMcp,
    toggleVoiceRecording,
    speakTextFromUi,
    refreshAudioMedia,
    runOcrFromUi,
    parseDocFromUi,
    sendDocToAgent,
    refreshComfyWorkflows,
    generateImageFromUi,
    refreshVisualMedia,
    upscaleGalleryItem,
    sendVisualMediaToAgent,
    revealGalleryItem,
    upscaleAssetPathFromUi,
    convertAssetWebpFromUi,
    inspectVideoFromUi,
    toggle3dWireframe,
    toggle3dGrid,
    reset3dCamera,
    createSample3dCubeFromUi,
    onSelect3dSourceImage,
    generate3dFromUi,
    refresh3dModels,
    refresh3dSourceImages,
    select3dGalleryItem,
    reveal3dGalleryItem,
    convertSelected3dWithBlender,
    renderSelected3dPreview,
    sendSelected3dToAgent,
    revealSelected3dInFinder,
    inspectUnityProjectFromUi,
    exportSelected3dToUnity,
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})(window);
