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
    // Phase 12: Autonomous Local Runtimes, Projects, Sessions, Attachments & Live RAM
    runtimes: [],
    activeLoadedModel: null,
    modelLoadingText: null,
    installingRuntime: null,
    pendingMissingRuntime: null,
    runtimeLogLines: [],
    customModelOpen: false,
    projects: [],
    activeProjectId: null,
    activeWorkspaceDir: '',
    projectSessions: [],
    activeSessionId: null,
    sessionMessages: [],
    attachments: [],
    liveMemory: null,
    memoryPollTimer: null,
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
  // Model Popover, Zero-Typing Grouped Model Selector & Composer Helpers
  // =========================================================================
  function toggleModelPopover() {
    const pop = el('model-popover');
    if (!pop) return;
    const willOpen = !pop.classList.contains('open');
    pop.classList.toggle('open', willOpen);
    if (willOpen) {
      refreshModelPickerOptions();
    }
  }

  function closeModelPopover() {
    const pop = el('model-popover');
    if (pop) pop.classList.remove('open');
  }

  function openModelConfig() {
    navigate('view-agent');
    const pop = el('model-popover');
    if (pop) {
      pop.classList.add('open');
      refreshModelPickerOptions();
    }
  }

  function toggleCustomModelInput() {
    state.customModelOpen = !state.customModelOpen;
    const box = el('custom-model-fields');
    const lbl = el('custom-model-toggle-label');
    if (box) box.style.display = state.customModelOpen ? 'flex' : 'none';
    if (lbl) lbl.textContent = state.customModelOpen ? 'Hide custom model ID' : 'Enter custom model ID...';
  }

  function syncModelLabels() {
    const provider = (el('sel-provider') && el('sel-provider').value) || 'dex-local';
    const customModel = (el('input-model') && el('input-model').value.trim()) || '';
    const dot = el('model-pill-dot');
    const cLabel = el('composer-model-label');
    const sLabel = el('sidebar-model-label');

    let text = 'Select local model...';
    let dotState = '';

    if (state.modelLoadingText) {
      text = state.modelLoadingText;
      dotState = 'loading';
    } else if (provider === 'dex-local' && state.activeLoadedModel) {
      const m = state.activeLoadedModel;
      const fmt = String(m.format || m.runtime || 'MLX').toUpperCase();
      const q = m.quantization ? ` ${m.quantization}` : '';
      text = `${m.name} (${fmt}${q})`;
      dotState = 'ready';
    } else if (customModel) {
      text = `${provider} / ${customModel}`;
      dotState = provider === 'openai' || provider === 'ollama' || provider === 'lmstudio' ? 'ready' : '';
    } else if (provider === 'dex-local') {
      text = 'Select local model...';
      dotState = '';
    } else {
      text = `${provider} / select model`;
      dotState = '';
    }

    if (dot) {
      dot.className = `model-pill-dot${dotState ? ' ' + dotState : ''}`;
    }
    if (cLabel) cLabel.textContent = text;
    if (sLabel) sLabel.textContent = text;
  }

  function onProviderChange() {
    const provider = el('sel-provider').value;
    const modelInput = el('input-model');
    const apiBaseInput = el('input-apibase');
    if (provider === 'openai') {
      if (!state.customModelOpen) toggleCustomModelInput();
      if (modelInput && !modelInput.value.trim()) modelInput.value = 'gpt-4o';
      if (apiBaseInput && (apiBaseInput.value.includes('127.0.0.1:808') || apiBaseInput.value.includes(':1234'))) {
        apiBaseInput.value = '';
      }
    } else if (provider === 'lmstudio') {
      if (apiBaseInput && !apiBaseInput.value.trim()) {
        apiBaseInput.value = 'http://127.0.0.1:1234/v1';
      }
    } else if (provider === 'ollama') {
      if (apiBaseInput && (apiBaseInput.value.includes(':1234') || apiBaseInput.value.includes(':808'))) {
        apiBaseInput.value = '';
      }
    } else if (provider === 'dex-local' && state.activeLoadedModel) {
      if (modelInput) modelInput.value = state.activeLoadedModel.path;
      if (apiBaseInput) apiBaseInput.value = state.activeLoadedModel.base_url;
    }
    renderModelPickerSelect();
    syncModelLabels();
  }

  async function refreshModelPickerOptions() {
    try {
      const [scanned, servers, activeLoaded, runtimes] = await Promise.all([
        invoke('scan_local_models'),
        invoke('detect_local_servers'),
        invoke('get_active_loaded_model'),
        invoke('detect_runtimes'),
      ]);
      state.scannedModels = scanned || [];
      state.localServers = servers || [];
      state.activeLoadedModel = activeLoaded || null;
      state.runtimes = runtimes || [];
      renderModelPickerSelect();
      renderRuntimesPanel();
      renderActiveLoadedBanner();
      syncModelLabels();
    } catch (err) {
      console.error('Model picker refresh failed:', err);
    }
  }

  function renderModelPickerSelect() {
    const picker = el('sel-model-picker');
    if (!picker) return;

    const localRunnable = (state.scannedModels || []).filter(
      (m) => (m.format === 'mlx' || m.format === 'gguf') && m.path
    );
    const ollamaSrv = (state.localServers || []).find((s) => s.id === 'ollama' && s.reachable);
    const lmSrv = (state.localServers || []).find((s) => s.id === 'lmstudio' && s.reachable);
    const ollamaModels = ollamaSrv ? ollamaSrv.loaded_models || [] : [];
    const lmModels = lmSrv ? lmSrv.loaded_models || [] : [];

    const parts = [];
    parts.push('<option value="">-- Choose a detected model --</option>');

    if (localRunnable.length > 0) {
      parts.push('<optgroup label="Dex Native Runtime (MLX &amp; GGUF)">');
      for (const m of localRunnable) {
        const fmt = m.format.toUpperCase();
        const q = m.quantization ? ` · ${m.quantization}` : '';
        const sz = formatSizeGb(m.size_bytes);
        const val = `local::${m.id}`;
        const isLoaded = state.activeLoadedModel && state.activeLoadedModel.path === m.path;
        const prefix = isLoaded ? '[Loaded] ' : '';
        parts.push(
          `<option value="${escapeHtml(val)}" ${isLoaded ? 'selected' : ''}>${escapeHtml(prefix + m.name)} [${fmt}${escapeHtml(q)} · ${escapeHtml(sz)}]</option>`
        );
      }
      parts.push('</optgroup>');
    }

    if (ollamaModels.length > 0) {
      parts.push('<optgroup label="Ollama (Running :11434)">');
      for (const name of ollamaModels) {
        const val = `ollama::${name}`;
        const isSel = el('sel-provider') && el('sel-provider').value === 'ollama' && el('input-model') && el('input-model').value === name;
        parts.push(`<option value="${escapeHtml(val)}" ${isSel ? 'selected' : ''}>${escapeHtml(name)} [Ollama]</option>`);
      }
      parts.push('</optgroup>');
    }

    if (lmModels.length > 0) {
      parts.push('<optgroup label="LM Studio / Bionic (Running :1234)">');
      for (const name of lmModels) {
        const val = `lmstudio::${name}`;
        const isSel = el('sel-provider') && el('sel-provider').value === 'lmstudio' && el('input-model') && el('input-model').value === name;
        parts.push(`<option value="${escapeHtml(val)}" ${isSel ? 'selected' : ''}>${escapeHtml(name)} [LM Studio]</option>`);
      }
      parts.push('</optgroup>');
    }

    picker.innerHTML = parts.join('');
    updatePopoverLoadedStatus();
  }

  function updatePopoverLoadedStatus() {
    const box = el('popover-loaded-status');
    const txt = el('popover-loaded-text');
    if (box && txt) {
      if (state.activeLoadedModel) {
        const m = state.activeLoadedModel;
        const fmt = String(m.format || m.runtime || '').toUpperCase();
        txt.textContent = `Mounted: ${m.name} (${fmt}${m.quantization ? ' ' + m.quantization : ''}) :${m.port}`;
        box.style.display = 'flex';
      } else {
        box.style.display = 'none';
      }
    }

    const missBox = el('popover-runtime-missing');
    const missTxt = el('popover-runtime-missing-text');
    const missBtn = el('btn-popover-install-rt');
    if (missBox && missTxt && missBtn) {
      if (state.pendingMissingRuntime) {
        const label = state.pendingMissingRuntime === 'mlx' ? 'MLX Runtime' : 'llama.cpp';
        missTxt.textContent = `${label} is not installed yet.`;
        missBtn.innerHTML = `${icon('download', 11)} <span>Install ${escapeHtml(label)} (1-click)</span>`;
        missBox.style.display = 'flex';
      } else {
        missBox.style.display = 'none';
      }
    }
  }

  async function onSelectModelFromPicker() {
    const picker = el('sel-model-picker');
    if (!picker || !picker.value) return;
    const raw = picker.value;

    if (raw.startsWith('local::')) {
      const id = raw.slice('local::'.length);
      const m = state.scannedModels.find((x) => x.id === id);
      if (!m) return;
      await loadLocalModelFromSpec(m, false);
      return;
    }

    if (raw.startsWith('ollama::')) {
      const name = raw.slice('ollama::'.length);
      el('sel-provider').value = 'ollama';
      el('input-model').value = name;
      el('input-apibase').value = '';
      state.pendingMissingRuntime = null;
      updatePopoverLoadedStatus();
      syncModelLabels();
      appendSystemNote(`Selected Ollama model: ${name}`);
      return;
    }

    if (raw.startsWith('lmstudio::')) {
      const name = raw.slice('lmstudio::'.length);
      const lmSrv = state.localServers.find((s) => s.id === 'lmstudio');
      el('sel-provider').value = 'lmstudio';
      el('input-model').value = name;
      el('input-apibase').value = (lmSrv && lmSrv.base_url) || 'http://127.0.0.1:1234/v1';
      state.pendingMissingRuntime = null;
      updatePopoverLoadedStatus();
      syncModelLabels();
      appendSystemNote(`Selected LM Studio model: ${name}`);
    }
  }

  async function installMissingRuntimeFromPopover() {
    if (!state.pendingMissingRuntime) return;
    const rt = state.pendingMissingRuntime;
    closeModelPopover();
    navigate('view-models');
    switchModelsTab('library');
    await installRuntime(rt);
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
  // Composer File Attachments (Native Dialog + Drag & Drop)
  // =========================================================================
  function iconForAttachmentKind(kind) {
    if (kind === 'image') return 'image';
    if (kind === 'document') return 'fileText';
    if (kind === 'mesh_3d') return 'cube';
    if (kind === 'audio') return 'mic';
    if (kind === 'video') return 'film';
    if (kind === 'code') return 'code';
    return 'file';
  }

  function renderComposerAttachments() {
    const box = el('composer-attachments');
    if (!box) return;
    if (!state.attachments || state.attachments.length === 0) {
      box.classList.remove('has-items');
      box.innerHTML = '';
      return;
    }
    box.classList.add('has-items');
    box.innerHTML = state.attachments.map((att, idx) => `
      <span class="att-chip" title="${escapeHtml(att.path)}">
        ${icon(iconForAttachmentKind(att.kind), 12)}
        <span class="att-chip-name">${escapeHtml(att.filename)}</span>
        <span class="att-chip-size">${escapeHtml(formatBytesKb(att.size_bytes || 0))}</span>
        <button class="att-chip-rm" title="Remove attachment" onclick="DexApp.removeAttachment(${idx})">${icon('x', 11)}</button>
      </span>
    `).join('');
  }

  function mergeAttachmentMetas(items) {
    if (!Array.isArray(items) || items.length === 0) return;
    const seen = new Set(state.attachments.map((a) => a.path));
    for (const item of items) {
      if (item && item.path && !seen.has(item.path)) {
        state.attachments.push(item);
        seen.add(item.path);
      }
    }
    renderComposerAttachments();
  }

  async function pickAttachmentFiles() {
    try {
      const files = await invoke('pick_attachment_files');
      mergeAttachmentMetas(files || []);
    } catch (err) {
      appendSystemNote(`Attachment picker error: ${err}`, 'error');
    }
  }

  async function addAttachmentPaths(paths) {
    if (!Array.isArray(paths) || paths.length === 0) return;
    try {
      const metas = await invoke('inspect_attachment_paths', { paths });
      mergeAttachmentMetas(metas || []);
    } catch (err) {
      appendSystemNote(`Could not attach dropped files: ${err}`, 'error');
    }
  }

  function removeAttachment(idx) {
    state.attachments.splice(idx, 1);
    renderComposerAttachments();
  }

  // =========================================================================
  // Projects / Workspaces & Persisted Sessions (`~/.dex/projects.json`)
  // =========================================================================
  async function refreshProjects() {
    try {
      const res = await invoke('list_projects');
      applyProjectsResponse(res);
      await refreshProjectSessions();
    } catch (err) {
      console.error('Failed to load projects:', err);
    }
  }

  function applyProjectsResponse(res) {
    if (!res) return;
    state.activeProjectId = res.active_project_id || null;
    state.activeWorkspaceDir = res.active_workspace_dir || '';
    state.projects = res.projects || [];
    renderSidebarProjects();
    const wsEl = el('active-workspace-path');
    if (wsEl) {
      const activeProj = state.projects.find((p) => p.id === state.activeProjectId);
      const label = activeProj ? `${activeProj.name} — ${activeProj.path}` : (state.activeWorkspaceDir || 'Default Workspace');
      wsEl.textContent = label;
      wsEl.parentElement && wsEl.parentElement.setAttribute('title', state.activeWorkspaceDir);
    }
  }

  function renderSidebarProjects() {
    const list = el('sidebar-projects-list');
    if (!list) return;
    if (!state.projects || state.projects.length === 0) {
      list.innerHTML = '<div class="card-meta" style="padding: 3px 8px; font-size: 11px;">No projects yet. Click New or Open.</div>';
      return;
    }
    list.innerHTML = state.projects.map((p) => {
      const active = p.id === state.activeProjectId;
      return `
        <div class="project-row ${active ? 'active' : ''}" title="${escapeHtml(p.path)}" onclick="DexApp.selectProject('${escapeHtml(p.id)}')">
          <div class="project-row-left">
            ${icon(active ? 'folderOpen' : 'folder', 13)}
            <span class="project-row-name">${escapeHtml(p.name)}</span>
          </div>
          <button class="project-row-rm" title="Remove from recent projects" onclick="event.stopPropagation(); DexApp.removeProject('${escapeHtml(p.id)}')">
            ${icon('x', 11)}
          </button>
        </div>
      `;
    }).join('');
  }

  function openCreateProjectModal() {
    const modal = el('create-project-modal');
    if (modal) modal.classList.add('open');
    const inp = el('proj-name-input');
    if (inp) inp.focus();
  }

  function closeCreateProjectModal() {
    const modal = el('create-project-modal');
    if (modal) modal.classList.remove('open');
  }

  async function submitCreateProject() {
    const name = (el('proj-name-input') && el('proj-name-input').value.trim()) || '';
    const parentDir = (el('proj-parent-input') && el('proj-parent-input').value.trim()) || null;
    const initGit = el('proj-init-git') ? !!el('proj-init-git').checked : true;
    if (!name) return alert('Enter a project name.');
    try {
      const res = await invoke('create_project', { name, parentDir, initGit });
      el('proj-name-input').value = '';
      closeCreateProjectModal();
      applyProjectsResponse(res);
      startNewChat(true);
      await refreshProjectSessions();
      refreshTasks();
      refreshScreenshots();
      appendSystemNote(`Created and activated project workspace: ${state.activeWorkspaceDir}`);
    } catch (err) {
      alert(`Failed to create project: ${err}`);
    }
  }

  async function openProjectDialog() {
    try {
      const res = await invoke('open_project_dialog');
      if (!res) return;
      applyProjectsResponse(res);
      startNewChat(true);
      await refreshProjectSessions();
      refreshTasks();
      refreshScreenshots();
      navigate('view-agent');
      appendSystemNote(`Workspace switched to: ${state.activeWorkspaceDir}`);
    } catch (err) {
      appendSystemNote(`Could not open folder: ${err}`, 'error');
    }
  }

  async function selectProject(id) {
    try {
      const res = await invoke('select_project', { id });
      applyProjectsResponse(res);
      startNewChat(true);
      await refreshProjectSessions();
      refreshTasks();
      refreshScreenshots();
      navigate('view-agent');
      appendSystemNote(`Active project workspace: ${state.activeWorkspaceDir}`);
    } catch (err) {
      appendSystemNote(`Could not select project: ${err}`, 'error');
    }
  }

  async function removeProject(id) {
    try {
      const res = await invoke('remove_project', { id });
      applyProjectsResponse(res);
      await refreshProjectSessions();
    } catch (err) {
      appendSystemNote(`Failed to remove project: ${err}`, 'error');
    }
  }

  function startNewChat(silent) {
    state.activeSessionId = `sess-${Date.now()}`;
    state.sessionMessages = [];
    clearAgentFeed();
    renderSidebarSessions();
    if (!silent) {
      navigate('view-agent');
      const ta = el('input-goal');
      if (ta) ta.focus();
    }
  }

  async function refreshProjectSessions() {
    try {
      const list = await invoke('list_project_sessions');
      state.projectSessions = list || [];
      renderSidebarSessions();
    } catch (_) {
      state.projectSessions = [];
      renderSidebarSessions();
    }
  }

  function renderSidebarSessions() {
    const box = el('sidebar-sessions-list');
    if (!box) return;
    if (!state.projectSessions || state.projectSessions.length === 0) {
      box.innerHTML = '';
      return;
    }
    box.innerHTML = state.projectSessions.slice(0, 6).map((s) => `
      <button class="session-row ${s.id === state.activeSessionId ? 'active' : ''}" title="${escapeHtml(s.title)}" onclick="DexApp.openProjectSession('${escapeHtml(s.id)}')">
        <span class="session-row-title">${escapeHtml(s.title)}</span>
        <span class="mono" style="font-size: 9.5px;">${s.message_count}</span>
      </button>
    `).join('');
  }

  async function openProjectSession(sessionId) {
    try {
      const session = await invoke('load_project_session', { sessionId });
      if (!session) return;
      state.activeSessionId = session.id;
      state.sessionMessages = session.messages || [];
      clearAgentFeed();
      navigate('view-agent');
      for (const msg of state.sessionMessages) {
        if (msg.role === 'user') {
          appendUserMessage(msg.content, (msg.attachments || []).map((p) => ({
            path: p,
            filename: p.split('/').pop() || p,
          })));
        } else if (msg.role === 'assistant') {
          appendFinalAnswer(msg.content);
        } else if (msg.role === 'system') {
          appendSystemNote(msg.content);
        }
      }
      renderSidebarSessions();
    } catch (err) {
      appendSystemNote(`Could not open session: ${err}`, 'error');
    }
  }

  async function persistCurrentSession() {
    if (!state.sessionMessages || state.sessionMessages.length === 0) return;
    if (!state.activeSessionId) {
      state.activeSessionId = `sess-${Date.now()}`;
    }
    const firstUser = state.sessionMessages.find((m) => m.role === 'user');
    const titleRaw = firstUser ? firstUser.content.trim() : 'Untitled Chat';
    const title = titleRaw.length > 48 ? `${titleRaw.slice(0, 48)}...` : (titleRaw || 'Untitled Chat');
    const nowSec = Math.floor(Date.now() / 1000);
    try {
      await invoke('save_project_session', {
        session: {
          id: state.activeSessionId,
          title,
          created_at: nowSec,
          updated_at: nowSec,
          messages: state.sessionMessages,
        },
      });
      await refreshProjectSessions();
    } catch (_) {}
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

  function appendUserMessage(text, attachedMetas) {
    hideEmptyState();
    state.currentThoughtEl = null;
    const feed = el('agent-feed');
    const div = document.createElement('div');
    div.className = 'chat-entry';
    const attHtml = Array.isArray(attachedMetas) && attachedMetas.length > 0
      ? `<div class="chat-user-atts">${attachedMetas.map((a) => `
          <span class="chat-user-att-tag" title="${escapeHtml(a.path || a.filename)}">
            ${icon(iconForAttachmentKind(a.kind || 'file'), 11)}
            <span>${escapeHtml(a.filename || a.path)}</span>
          </span>`).join('')}</div>`
      : '';
    div.innerHTML = `<div class="chat-user">${escapeHtml(text || '(Attached files)')}${attHtml}</div>`;
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
    const pendingAttachments = state.attachments.slice();
    if ((!goal && pendingAttachments.length === 0) || state.running) return;

    const provider = el('sel-provider').value;
    const model = el('input-model') ? el('input-model').value.trim() : '';
    const apiBase = (el('input-apibase') && el('input-apibase').value.trim()) || null;
    const attachmentPaths = pendingAttachments.map((a) => a.path);

    closeModelPopover();
    appendUserMessage(goal, pendingAttachments);
    state.sessionMessages.push({
      role: 'user',
      content: goal || '(Attached files)',
      attachments: attachmentPaths,
      timestamp: Math.floor(Date.now() / 1000),
    });
    persistCurrentSession();

    ta.value = '';
    state.attachments = [];
    renderComposerAttachments();
    autoResizeComposer();
    setRunningState(true);

    try {
      await invoke('run_goal', {
        goal,
        mode: state.mode,
        provider,
        model: model || null,
        apiBase,
        attachments: attachmentPaths.length > 0 ? attachmentPaths : null,
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
  // Live macOS RAM Telemetry & Hardware Cookbook
  // =========================================================================
  function applyLiveMemoryUi(mem) {
    if (!mem || mem.total_memory_gb === undefined) return;
    state.liveMemory = mem;

    const used = Number(mem.used_memory_gb || 0);
    const total = Number(mem.total_memory_gb || 0);
    const avail = Number(mem.available_memory_gb || 0);
    const pct = Number(mem.usage_percent || 0);

    const sidebarRam = el('sidebar-ram-metric');
    if (sidebarRam) {
      sidebarRam.textContent = `${used.toFixed(1)} / ${total.toFixed(1)} GB · ${Math.round(pct)}%`;
    }
    const bar = el('sidebar-ram-bar-fill');
    if (bar) {
      bar.style.width = `${Math.min(100, Math.max(0, pct))}%`;
      bar.className = `sidebar-ram-bar-fill${pct >= 85 ? ' high' : pct >= 70 ? ' warn' : ''}`;
    }

    // Also keep Cookbook live stat tiles synchronized in real time
    const tileUsed = el('cookbook-mem-used');
    const tileAvail = el('cookbook-mem-avail');
    const tileTotal = el('cookbook-mem-total');
    const tilePct = el('cookbook-mem-pct');
    if (tileUsed) tileUsed.textContent = `${used.toFixed(1)} GB`;
    if (tileAvail) tileAvail.textContent = `${avail.toFixed(1)} GB`;
    if (tileTotal) tileTotal.textContent = `${total.toFixed(1)} GB`;
    if (tilePct) tilePct.textContent = `${pct.toFixed(1)}% in use (Active + Wired + Compressed)`;
  }

  async function pollLiveMemory() {
    if (!global.__TAURI__) return;
    try {
      const mem = await invoke('get_live_memory');
      if (mem) applyLiveMemoryUi(mem);
    } catch (_) {}
  }

  function startLiveMemoryPolling() {
    if (state.memoryPollTimer) clearInterval(state.memoryPollTimer);
    pollLiveMemory();
    state.memoryPollTimer = setInterval(pollLiveMemory, 3000);
  }

  async function refreshCookbook() {
    try {
      const res = await invoke('get_cookbook');
      if (!res) return;
      const reportEl = el('cookbook-report');
      if (reportEl && res.report) reportEl.textContent = res.report;

      const p = res.profile || {};
      const totalMem = Number(p.total_memory_gb !== undefined ? p.total_memory_gb : (p.total_ram_gb || 0));
      const usedMem = Number(p.used_memory_gb !== undefined ? p.used_memory_gb : Math.max(0, totalMem - Number(p.available_memory_gb || 0)));
      const availMem = Number(p.available_memory_gb !== undefined ? p.available_memory_gb : (p.available_ram_gb || 0));
      const pct = Number(p.usage_percent !== undefined ? p.usage_percent : (totalMem > 0 ? (usedMem / totalMem) * 100 : 0));

      applyLiveMemoryUi({
        total_memory_gb: totalMem,
        used_memory_gb: usedMem,
        available_memory_gb: availMem,
        usage_percent: pct,
      });

      const statsEl = el('cookbook-stats');
      if (statsEl) {
        const topRec = (res.recommendations && res.recommendations[0])
          ? res.recommendations[0].tier || res.recommendations[0].model_name
          : 'Detected';
        const platformLabel = p.is_apple_silicon ? 'Apple Silicon' : (p.os_name || 'Desktop');
        statsEl.innerHTML = `
          <div class="stat-tile">
            <span class="stat-label">Used Memory</span>
            <span id="cookbook-mem-used" class="stat-value mono">${usedMem.toFixed(1)} GB</span>
            <span id="cookbook-mem-pct" class="card-meta">${pct.toFixed(1)}% in use (Active + Wired + Compressed)</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">Available Memory</span>
            <span id="cookbook-mem-avail" class="stat-value mono" style="color: var(--ok-fg);">${availMem.toFixed(1)} GB</span>
            <span class="card-meta">Reclaimable for MLX / GGUF models</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">Total Memory</span>
            <span id="cookbook-mem-total" class="stat-value mono">${totalMem.toFixed(1)} GB</span>
            <span class="card-meta">${p.is_apple_silicon ? 'Unified Memory Architecture' : 'System RAM'}</span>
          </div>
          <div class="stat-tile">
            <span class="stat-label">Platform &amp; CPU</span>
            <span class="stat-value">${escapeHtml(platformLabel)}</span>
            <span class="card-meta mono">${escapeHtml(p.cpu_cores || 'n/a')} cores · ${escapeHtml(p.arch || '')} · ${escapeHtml(topRec)}</span>
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
      listEl.innerHTML = '<div class="empty-note">Scanning model directories and local runtimes...</div>';
    }
    try {
      const [dirs, scanned, servers, runtimes, activeLoaded] = await Promise.all([
        invoke('list_model_dirs'),
        invoke('scan_local_models'),
        invoke('detect_local_servers'),
        invoke('detect_runtimes'),
        invoke('get_active_loaded_model'),
      ]);
      state.modelDirs = dirs || [];
      state.scannedModels = scanned || [];
      state.localServers = servers || [];
      state.runtimes = runtimes || [];
      state.activeLoadedModel = activeLoaded || null;
      renderRuntimesPanel();
      renderActiveLoadedBanner();
      renderModelDirsStrip();
      renderLocalServersStrip();
      renderLibraryModels();
      renderModelPickerSelect();
      syncModelLabels();
    } catch (err) {
      if (listEl) listEl.innerHTML = `<div class="empty-note" style="color: var(--err-fg);">Scan error: ${escapeHtml(err)}</div>`;
    }
  }

  function renderRuntimesPanel() {
    const container = el('native-runtime-cards');
    if (!container) return;
    if (!state.runtimes || state.runtimes.length === 0) {
      container.innerHTML = '';
      return;
    }
    container.innerHTML = state.runtimes.map((rt) => {
      const installing = state.installingRuntime === rt.id;
      const dotStyle = rt.available ? 'background: var(--ok-fg);' : 'background: var(--warn-fg);';
      const sub = rt.available
        ? (rt.binary_path || 'Ready')
        : (installing ? 'Installing isolated runtime...' : rt.install_hint);
      const btnLabel = rt.id === 'mlx' ? 'Install MLX Runtime (1-click)' : 'Install llama.cpp (1-click)';
      const actionHtml = rt.available
        ? '<span class="badge badge-ok">Ready</span>'
        : `<button class="btn btn-sm btn-primary" ${installing ? 'disabled' : ''} onclick="DexApp.installRuntime('${escapeHtml(rt.id)}')">
             ${icon('download', 12)} <span>${installing ? 'Installing...' : escapeHtml(btnLabel)}</span>
           </button>`;
      return `
        <div class="runtime-card ${rt.available ? 'ready' : ''}" title="${escapeHtml(rt.binary_path || rt.managed_dir || '')}">
          <div class="runtime-card-info">
            <div class="runtime-card-title">
              <span class="status-dot" style="${dotStyle}"></span>
              <span>${escapeHtml(rt.name)}</span>
            </div>
            <div class="runtime-card-sub">${escapeHtml(sub)}</div>
          </div>
          ${actionHtml}
        </div>
      `;
    }).join('');
  }

  function renderActiveLoadedBanner() {
    const banner = el('active-loaded-banner');
    const titleEl = el('active-loaded-title');
    const metaEl = el('active-loaded-meta');
    if (!banner) return;
    const m = state.activeLoadedModel;
    if (!m) {
      banner.classList.remove('visible');
      updatePopoverLoadedStatus();
      return;
    }
    banner.classList.add('visible');
    const fmt = String(m.format || m.runtime || '').toUpperCase();
    const q = m.quantization ? ` · ${m.quantization}` : '';
    const sz = m.size_bytes ? ` · ${formatSizeGb(m.size_bytes)}` : '';
    const pid = m.pid ? ` · PID ${m.pid}` : '';
    if (titleEl) titleEl.textContent = `${m.name} (${fmt}${q})`;
    if (metaEl) metaEl.textContent = `Dex Native Runtime · ${m.base_url}${sz}${pid}`;
    updatePopoverLoadedStatus();
  }

  function appendRuntimeLog(line, isError) {
    const box = el('runtime-log-box');
    if (!box) return;
    if (!line) return;
    state.runtimeLogLines.push(line);
    if (state.runtimeLogLines.length > 60) {
      state.runtimeLogLines = state.runtimeLogLines.slice(-60);
    }
    box.classList.add('visible');
    box.textContent = state.runtimeLogLines.join('\n');
    box.style.borderColor = isError ? 'var(--err-border)' : 'var(--border-strong)';
    box.scrollTop = box.scrollHeight;
  }

  async function installRuntime(runtimeId) {
    if (state.installingRuntime) return;
    state.installingRuntime = runtimeId;
    state.runtimeLogLines = [`Starting 1-click installation for ${runtimeId}...`];
    appendRuntimeLog('Creating isolated environment (system Python untouched)...', false);
    renderRuntimesPanel();
    try {
      const status = await invoke('install_runtime', { runtime: runtimeId });
      state.installingRuntime = null;
      state.pendingMissingRuntime = null;
      appendRuntimeLog(`Installation complete: ${status.binary_path || status.name}`, false);
      const noteEl = el('library-status-note');
      if (noteEl) {
        noteEl.style.display = 'block';
        noteEl.innerHTML = `<span style="color: var(--ok-fg);">${escapeHtml(status.name)} is installed and ready. Click Use Model on any local model.</span>`;
      }
      await refreshModelsLibrary();
    } catch (err) {
      state.installingRuntime = null;
      appendRuntimeLog(`Installation error: ${err}`, true);
      renderRuntimesPanel();
    }
  }

  async function loadLocalModelFromSpec(m, switchToAgent) {
    if (!m || !m.path) return;
    const noteEl = el('library-status-note');
    const sizeText = m.size_bytes ? formatSizeGb(m.size_bytes) : 'model';
    state.modelLoadingText = `Loading ${sizeText}...`;
    state.pendingMissingRuntime = null;
    state.runtimeLogLines = [`Loading ${m.name} (${m.format.toUpperCase()} ${m.quantization || ''}) into Unified Memory...`];
    syncModelLabels();
    if (noteEl) {
      noteEl.style.display = 'block';
      noteEl.textContent = `Loading '${m.name}' (${sizeText}) via Dex Native Runtime (up to 90s for large weights)...`;
    }
    appendRuntimeLog(`Spawning Dex Native Runtime for ${m.path}...`, false);

    try {
      const res = await invoke('load_local_model', {
        modelId: m.id,
        modelName: m.name,
        format: m.format,
        modelPath: m.path,
        quantization: m.quantization || null,
        sizeBytes: m.size_bytes || null,
      });

      state.modelLoadingText = null;

      if (res && res.status === 'runtime_missing') {
        state.pendingMissingRuntime = res.runtime;
        const btnLabel = res.runtime === 'mlx' ? 'Install MLX Runtime (1-click)' : 'Install llama.cpp (1-click)';
        if (noteEl) {
          noteEl.style.display = 'block';
          noteEl.innerHTML = `
            <div class="row-wrap" style="gap: 10px; align-items: center;">
              <span style="color: var(--warn-fg);">${escapeHtml(res.message)}</span>
              <button class="btn btn-sm btn-primary" onclick="DexApp.installRuntime('${escapeHtml(res.runtime)}')">
                ${icon('download', 12)} <span>${escapeHtml(btnLabel)}</span>
              </button>
            </div>
          `;
        }
        appendRuntimeLog(res.message, true);
        updatePopoverLoadedStatus();
        syncModelLabels();
        return;
      }

      if (res && res.status === 'loaded' && res.model) {
        state.activeLoadedModel = res.model;
        el('sel-provider').value = 'dex-local';
        if (el('input-model')) el('input-model').value = res.model.path;
        if (el('input-apibase')) el('input-apibase').value = res.model.base_url;
        if (noteEl) noteEl.style.display = 'none';
        renderActiveLoadedBanner();
        renderModelPickerSelect();
        renderLibraryModels();
        syncModelLabels();
        pollLiveMemory();
        if (switchToAgent) {
          navigate('view-agent');
          el('input-goal') && el('input-goal').focus();
        }
        appendSystemNote(res.message || `Loaded ${res.model.name} in Dex Native Runtime`);
      }
    } catch (err) {
      state.modelLoadingText = null;
      syncModelLabels();
      if (noteEl) {
        noteEl.style.display = 'block';
        noteEl.style.color = 'var(--err-fg)';
        noteEl.textContent = String(err);
      }
      appendRuntimeLog(`Model load failed: ${err}`, true);
      appendSystemNote(`Failed to load local model: ${err}`, 'error');
    }
  }

  async function unloadLocalModel() {
    try {
      const msg = await invoke('unload_local_model');
      state.activeLoadedModel = null;
      state.modelLoadingText = null;
      renderActiveLoadedBanner();
      renderModelPickerSelect();
      renderLibraryModels();
      syncModelLabels();
      appendSystemNote(msg || 'Unloaded local model from Unified Memory');
      setTimeout(pollLiveMemory, 600);
    } catch (err) {
      appendSystemNote(`Unload failed: ${err}`, 'error');
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

      const isLoaded = state.activeLoadedModel && m.path && state.activeLoadedModel.path === m.path;
      const actionBtn = isLoaded
        ? `<button class="btn btn-sm btn-danger" onclick="DexApp.unloadLocalModel()">
             ${icon('power', 12)} <span>Unload</span>
           </button>`
        : `<button class="btn btn-sm btn-primary" onclick="DexApp.useLibraryModel('${escapeHtml(m.id)}')">
             ${icon('zap', 12)} <span>Use Model</span>
           </button>`;

      return `
        <div class="library-row">
          <div class="library-row-main">
            <div class="library-row-head">
              ${isLoaded ? '<span class="model-pill-dot ready" title="Loaded in Dex Native Runtime"></span>' : ''}
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
            ${actionBtn}
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

    // Autonomous Dex Native Runtime for MLX (mlx_lm.server) & GGUF (llama-server -ngl 99)
    if ((m.format === 'mlx' || m.format === 'gguf') && m.path) {
      await loadLocalModelFromSpec(m, true);
      return;
    }

    // PT / other local files: attach to LM Studio if loaded, else prefill
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

  function orgMonogram(name) {
    const clean = String(name || 'HF').replace(/[^a-zA-Z0-9\s_-]/g, ' ').trim();
    if (!clean) return 'HF';
    const parts = clean.split(/[\s_-]+/).filter(Boolean);
    if (parts.length >= 2) {
      return (parts[0][0] + parts[1][0]).toUpperCase();
    }
    return clean.slice(0, 2).toUpperCase();
  }

  function renderOrgAvatar(org, avatarUrl, sizeClass) {
    const mono = escapeHtml(orgMonogram(org));
    const safeUrl = avatarUrl && /^(https?:\/\/|data:image\/)/i.test(avatarUrl) ? avatarUrl : '';
    const imgHtml = safeUrl
      ? `<img src="${escapeHtml(safeUrl)}" alt="${escapeHtml(org || '')}" loading="lazy" onerror="this.remove()" />`
      : '';
    return `<span class="org-avatar-wrap ${sizeClass || 'org-avatar-sm'}" title="${escapeHtml(org || '')}"><span>${mono}</span>${imgHtml}</span>`;
  }

  function renderHfRepoList() {
    const container = el('hf-search-results');
    if (!container) return;
    container.innerHTML = state.hfReposCache.map((r, idx) => {
      const parts = r.id.split('/');
      const title = parts.length > 1 ? parts.slice(1).join('/') : r.id;
      const org = parts.length > 1 ? parts[0] : (r.author || '');
      const isAct = r.id === state.selectedHfRepoId;
      const avatarHtml = renderOrgAvatar(org, r.avatar_url, 'org-avatar-sm');
      const fmts = (r.formats || []).map((f) => `<span class="badge ${formatBadgeClass(f)} mono" style="padding: 1px 5px; font-size: 9.5px;">${escapeHtml(f)}</span>`).join('');
      const caps = (r.capabilities || []).slice(0, 2).map((c) => {
        const ic = c === 'Vision' ? 'eye' : c === 'Reasoning' ? 'brain' : 'wrench';
        return `<span class="cap-chip" style="font-size: 9.5px;">${icon(ic, 10)}</span>`;
      }).join('');
      const relDate = formatRelativeDate(r.last_modified);
      return `
        <button class="repo-item ${isAct ? 'active' : ''}" onclick="DexApp.selectHfRepo(${idx})">
          <div class="repo-item-top">
            ${avatarHtml}
            <div class="repo-item-meta">
              <div class="repo-item-title" title="${escapeHtml(r.id)}">${escapeHtml(title)}</div>
              <div class="repo-item-org">${escapeHtml(org)}</div>
            </div>
          </div>
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
    const org = repo.author || parts[0] || '';
    const avatarLg = renderOrgAvatar(org, repo.avatar_url, 'org-avatar-lg');
    const hfUrl = `https://huggingface.co/${repo.id}`;
    const fmts = (repo.formats || []).map((f) => `<span class="badge ${formatBadgeClass(f)} mono">${escapeHtml(f)}</span>`).join('');
    const caps = renderCapabilityChips(repo.capabilities || []);

    detailEl.innerHTML = `
      <div class="item-card-row" style="align-items: flex-start;">
        <div class="row" style="gap: 12px; align-items: center; min-width: 0;">
          ${avatarLg}
          <div style="min-width: 0;">
            <h2 style="font-size: 18px; font-weight: 600; letter-spacing: -0.02em;">${escapeHtml(title)}</h2>
            <div class="mono card-meta" style="margin-top: 2px;">${escapeHtml(repo.id)} · ${(repo.downloads || 0).toLocaleString()} downloads · ${(repo.likes || 0).toLocaleString()} likes</div>
          </div>
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
          <span class="card-meta">Author: <strong style="color: var(--text-primary);">${escapeHtml(org || '-')}</strong></span>
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
        readmeBody.innerHTML = renderSafeMarkdown(readmeRes.value, currentRepoId);
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

  function resolveHfImageUrl(repoId, rawSrc) {
    const trimmed = String(rawSrc || '').trim();
    if (!trimmed) return '';
    if (/^javascript:/i.test(trimmed) || /^vbscript:/i.test(trimmed)) return '';
    if (/^(https?:\/\/|data:image\/)/i.test(trimmed)) return trimmed;
    if (trimmed.startsWith('//')) return `https:${trimmed}`;
    const cleanRel = trimmed.replace(/^\.\//, '').replace(/^\/+/, '');
    const cleanRepo = String(repoId || '').trim().replace(/^\/+|\/+$/g, '');
    if (!cleanRepo) return '';
    return `https://huggingface.co/${cleanRepo}/resolve/main/${cleanRel}`;
  }

  function resolveHfLinkUrl(repoId, rawHref) {
    const trimmed = String(rawHref || '').trim();
    if (!trimmed || /^javascript:/i.test(trimmed) || trimmed.startsWith('#')) return '';
    if (/^https?:\/\//i.test(trimmed)) return trimmed;
    if (trimmed.startsWith('//')) return `https:${trimmed}`;
    const cleanRel = trimmed.replace(/^\.\//, '').replace(/^\/+/, '');
    const cleanRepo = String(repoId || '').trim().replace(/^\/+|\/+$/g, '');
    if (!cleanRepo) return '';
    return `https://huggingface.co/${cleanRepo}/blob/main/${cleanRel}`;
  }

  // Safe Full GFM Markdown Renderer (GFM Tables, Relative/Absolute HF Images, Blockquotes, Nested Lists, Code)
  function renderSafeMarkdown(md, repoId) {
    const lines = String(md || '').replace(/\r\n/g, '\n').split('\n');
    const out = [];
    let inCode = false;
    let codeBuf = [];
    let listStack = []; // entries: { tag: 'ul'|'ol', indent: number }
    let inBlockquote = false;

    function closeLists() {
      while (listStack.length > 0) {
        const top = listStack.pop();
        out.push(`</${top.tag}>`);
      }
    }

    function closeBlockquote() {
      if (inBlockquote) {
        out.push('</blockquote>');
        inBlockquote = false;
      }
    }

    function splitTableRow(rowStr) {
      let s = rowStr.trim();
      if (s.startsWith('|')) s = s.slice(1);
      if (s.endsWith('|')) s = s.slice(0, -1);
      return s.split('|').map((c) => c.trim());
    }

    function isTableSeparator(rowStr) {
      const t = rowStr.trim();
      if (!t.includes('-')) return false;
      const cells = splitTableRow(t);
      return cells.length >= 1 && cells.every((c) => /^:?-{2,}:?$/.test(c));
    }

    function formatInline(raw) {
      // 1. Extract safe HTML <img ...> tags into placeholders before HTML-escaping raw text
      const tokens = [];
      let work = String(raw || '').replace(/<img\b([^>]*?)\/?>/gi, (_, attrs) => {
        const srcMatch = attrs.match(/\bsrc\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s>]+))/i);
        const altMatch = attrs.match(/\balt\s*=\s*(?:"([^"]*)"|'([^']*)')/i);
        const rawSrc = srcMatch ? (srcMatch[1] || srcMatch[2] || srcMatch[3] || '') : '';
        const alt = altMatch ? (altMatch[1] || altMatch[2] || '') : '';
        const resolved = resolveHfImageUrl(repoId, rawSrc);
        if (!resolved) return '';
        const idx = tokens.length;
        tokens.push(`<img src="${escapeHtml(resolved)}" alt="${escapeHtml(alt)}" loading="lazy" onerror="this.style.display='none'" />`);
        return `\u0000IMG${idx}\u0000`;
      });

      // 2. Escape all remaining raw HTML to guarantee zero XSS
      let s = escapeHtml(work);

      // 3. Inline code spans (protect contents from bold/italic/image replacements)
      s = s.replace(/`([^`]+)`/g, (_, code) => {
        const idx = tokens.length;
        tokens.push(`<code>${code}</code>`);
        return `\u0000TOK${idx}\u0000`;
      });

      // 4. Markdown images ![alt](url) (including linked badge images [![alt](img)](href))
      s = s.replace(/\[!\[([^\]]*)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)\]\(([^)\s]+)\)/g, (_, alt, imgUrl, href) => {
        const resolvedImg = resolveHfImageUrl(repoId, imgUrl);
        const resolvedHref = resolveHfLinkUrl(repoId, href);
        if (!resolvedImg) return '';
        const imgTag = `<img src="${escapeHtml(resolvedImg)}" alt="${alt}" loading="lazy" onerror="this.style.display='none'" />`;
        if (!resolvedHref) return imgTag;
        const idx = tokens.length;
        tokens.push(`<a href="#" onclick="event.preventDefault(); DexApp.openExternalUrl('${escapeHtml(resolvedHref)}')">${imgTag}</a>`);
        return `\u0000TOK${idx}\u0000`;
      });

      s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)/g, (_, alt, imgUrl) => {
        const resolved = resolveHfImageUrl(repoId, imgUrl);
        if (!resolved) return '';
        const idx = tokens.length;
        tokens.push(`<img src="${escapeHtml(resolved)}" alt="${alt}" loading="lazy" onerror="this.style.display='none'" />`);
        return `\u0000TOK${idx}\u0000`;
      });

      // 5. Markdown links [label](url)
      s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, rawUrl) => {
        const resolved = resolveHfLinkUrl(repoId, rawUrl);
        if (!resolved) return label;
        const idx = tokens.length;
        tokens.push(`<a href="#" onclick="event.preventDefault(); DexApp.openExternalUrl('${escapeHtml(resolved)}')">${label}</a>`);
        return `\u0000TOK${idx}\u0000`;
      });

      // 6. Bold & italic
      s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
      s = s.replace(/(^|[^*])\*([^*]+)\*(?!\*)/g, '$1<em>$2</em>');

      // 7. Restore safe HTML tokens
      s = s.replace(/\u0000(?:IMG|TOK)(\d+)\u0000/g, (_, idxStr) => tokens[Number(idxStr)] || '');
      return s;
    }

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const trimmed = line.trim();

      // Fenced code blocks
      if (trimmed.startsWith('```')) {
        closeLists();
        closeBlockquote();
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

      if (!trimmed) {
        closeLists();
        closeBlockquote();
        continue;
      }

      // Skip standalone HTML wrapper tags like <div align="center">, </p>, <br/> cleanly
      if (/^<\/?(?:div|p|center|section|details|summary|table|thead|tbody|tr|td|th|br|hr|span|h[1-6]|a)\b[^>]*>$/i.test(trimmed)) {
        if (/^<br\s*\/?>$/i.test(trimmed)) out.push('<br />');
        continue;
      }

      // Horizontal rule
      if (/^(?:---+|\*\*\*+|___+)$/.test(trimmed)) {
        closeLists();
        closeBlockquote();
        out.push('<hr />');
        continue;
      }

      // GFM Table detection: header line containing `|` + next line is separator `|---|---|`
      if (trimmed.includes('|') && i + 1 < lines.length && isTableSeparator(lines[i + 1])) {
        closeLists();
        closeBlockquote();
        const headerCells = splitTableRow(trimmed);
        const bodyRows = [];
        i += 2; // consume header + separator
        while (i < lines.length) {
          const rowTrim = lines[i].trim();
          if (!rowTrim || !rowTrim.includes('|')) {
            i -= 1;
            break;
          }
          bodyRows.push(splitTableRow(rowTrim));
          i += 1;
        }
        const thead = `<thead><tr>${headerCells.map((c) => `<th>${formatInline(c)}</th>`).join('')}</tr></thead>`;
        const tbody = `<tbody>${bodyRows.map((row) => {
          const cells = [];
          for (let col = 0; col < Math.max(headerCells.length, row.length); col++) {
            cells.push(`<td>${formatInline(row[col] || '')}</td>`);
          }
          return `<tr>${cells.join('')}</tr>`;
        }).join('')}</tbody>`;
        out.push(`<div class="md-table-wrap"><table class="md-table">${thead}${tbody}</table></div>`);
        continue;
      }

      // Headings
      const hMatch = trimmed.match(/^(#{1,4})\s+(.*)$/);
      if (hMatch) {
        closeLists();
        closeBlockquote();
        const lvl = hMatch[1].length;
        out.push(`<h${lvl}>${formatInline(hMatch[2])}</h${lvl}>`);
        continue;
      }

      // Blockquotes (`> ...`)
      const bqMatch = trimmed.match(/^>\s?(.*)$/);
      if (bqMatch) {
        closeLists();
        if (!inBlockquote) {
          out.push('<blockquote>');
          inBlockquote = true;
        }
        out.push(`<p>${formatInline(bqMatch[1])}</p>`);
        continue;
      }
      closeBlockquote();

      // Unordered & ordered nested lists
      const ulMatch = line.match(/^(\s*)[-*+]\s+(.*)$/);
      const olMatch = !ulMatch ? line.match(/^(\s*)\d+\.\s+(.*)$/) : null;
      if (ulMatch || olMatch) {
        const indent = (ulMatch ? ulMatch[1] : olMatch[1]).length;
        const tag = ulMatch ? 'ul' : 'ol';
        const content = ulMatch ? ulMatch[2] : olMatch[2];
        while (listStack.length > 0 && indent < listStack[listStack.length - 1].indent) {
          out.push(`</${listStack.pop().tag}>`);
        }
        if (
          listStack.length === 0 ||
          indent > listStack[listStack.length - 1].indent ||
          listStack[listStack.length - 1].tag !== tag
        ) {
          out.push(`<${tag}>`);
          listStack.push({ tag, indent });
        }
        out.push(`<li>${formatInline(content)}</li>`);
        continue;
      }

      closeLists();
      out.push(`<p>${formatInline(trimmed)}</p>`);
    }

    if (inCode) out.push(`<pre><code>${escapeHtml(codeBuf.join('\n'))}</code></pre>`);
    closeLists();
    closeBlockquote();
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
      } else if (ev.type === 'RuntimeProgress') {
        appendRuntimeLog(`[${d.runtime || 'runtime'}:${d.stage || 'log'}] ${d.message || ''}`, !!d.is_error);
      }
    });

    listen('dex-files-dropped', (event) => {
      const paths = event.payload;
      if (Array.isArray(paths) && paths.length > 0) {
        addAttachmentPaths(paths);
        const box = el('composer-box');
        if (box) box.classList.remove('drag-over');
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
        const ans = data.answer || 'Objective completed.';
        appendFinalAnswer(ans);
        state.sessionMessages.push({
          role: 'assistant',
          content: ans,
          attachments: [],
          timestamp: Math.floor(Date.now() / 1000),
        });
        persistCurrentSession();
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

    // Drag & Drop highlight & fallback path extraction on Agent Composer
    const composerBox = el('composer-box');
    if (composerBox) {
      composerBox.addEventListener('dragover', (e) => {
        e.preventDefault();
        composerBox.classList.add('drag-over');
      });
      composerBox.addEventListener('dragleave', (e) => {
        if (!composerBox.contains(e.relatedTarget)) {
          composerBox.classList.remove('drag-over');
        }
      });
      composerBox.addEventListener('drop', (e) => {
        e.preventDefault();
        composerBox.classList.remove('drag-over');
        const files = e.dataTransfer && e.dataTransfer.files;
        if (files && files.length > 0) {
          const droppedPaths = [];
          for (let i = 0; i < files.length; i++) {
            if (files[i].path) droppedPaths.push(files[i].path);
          }
          if (droppedPaths.length > 0) {
            addAttachmentPaths(droppedPaths);
          }
        }
      });
    }

    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        closeModelPopover();
        closeCustomMcpModal();
        closeCreateProjectModal();
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

    // Sync initial execution mode, live RAM telemetry, projects, and detected models
    if (global.__TAURI__) {
      try {
        const mode = await invoke('get_mode');
        if (mode) updateModeButtons(mode);
      } catch (_) {}
      startLiveMemoryPolling();
      refreshCookbook();
      refreshProjects();
      refreshModelPickerOptions();
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
    toggleCustomModelInput,
    syncModelLabels,
    onProviderChange,
    refreshModelPickerOptions,
    onSelectModelFromPicker,
    installMissingRuntimeFromPopover,
    useQuickPrompt,
    clearAgentFeed,
    pickAttachmentFiles,
    addAttachmentPaths,
    removeAttachment,
    refreshProjects,
    openCreateProjectModal,
    closeCreateProjectModal,
    submitCreateProject,
    openProjectDialog,
    selectProject,
    removeProject,
    startNewChat,
    refreshProjectSessions,
    openProjectSession,
    setMode,
    runGoal,
    stopGoal,
    resolveApproval,
    refreshTasks,
    refreshScreenshots,
    pollLiveMemory,
    refreshCookbook,
    refreshInstalledModels,
    initModelsHubView,
    switchModelsTab,
    toggleModelsDrawer,
    closeModelsDrawers,
    refreshModelsLibrary,
    installRuntime,
    loadLocalModelFromSpec,
    unloadLocalModel,
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
    resolveHfImageUrl,
    renderSafeMarkdown,
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
