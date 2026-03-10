# Plan 3: Frontend UI — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the settings panel, setup wizard, and debate control bar so users can configure API keys, create debates, and control them from the UI.

**Architecture:** Everything added to the existing single-file `src/index.html`. Settings panel and wizard are modal overlays (same pattern as shortcut overlay). Debate control bar conditionally renders in the header when a debate is active. All data flows through tauri invoke commands (no direct API calls from JS).

**Tech Stack:** Vanilla JS, CSS, Tauri 2 invoke/listen

**Depends on:** Plan 1 (provider + config) and Plan 2 (orchestrator) must be complete.

**Spec:** `docs/superpowers/specs/2026-03-10-multi-model-orchestration-design.md`

---

## Chunk 1: Settings Panel

### Task 1: Settings panel — API key management

**Files:**
- Modify: `src/index.html` (CSS, HTML, JS)

**Context:** A gear icon in the header opens a modal where users enter API keys per provider. A "test" button per provider calls `list_models` to verify the key works. Keys are persisted via `save_config` tauri command.

- [ ] **Step 1: Add settings CSS**

After the shortcut overlay CSS rules, add:

```css
  .modal-overlay {
    display: none; position: fixed; inset: 0; z-index: 30;
    background: rgba(0,0,0,0.6); align-items: center; justify-content: center;
  }
  .modal-overlay.visible { display: flex; }
  .modal-panel {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 20px 28px; min-width: 400px; max-width: 560px;
    max-height: 80vh; overflow-y: auto;
  }
  .modal-panel h3 {
    font-size: 12px; font-weight: 600; margin-bottom: 14px;
    color: var(--text); text-transform: lowercase;
  }
  .field-group { margin-bottom: 12px; }
  .field-label {
    display: block; font-size: 10px; color: var(--muted);
    margin-bottom: 3px; text-transform: lowercase;
  }
  .field-row { display: flex; gap: 6px; align-items: center; }
  .field-input {
    flex: 1; background: var(--bg); border: 1px solid var(--border);
    color: var(--text); font-family: inherit; font-size: 11px;
    padding: 4px 8px; border-radius: 4px; outline: none;
  }
  .field-input:focus { border-color: var(--cyan); }
  .field-input::placeholder { color: var(--muted); }
  .btn-sm {
    background: transparent; border: 1px solid var(--border); color: var(--muted);
    font-family: inherit; font-size: 9px; padding: 3px 10px; border-radius: 4px;
    cursor: pointer; white-space: nowrap; transition: color 0.15s, border-color 0.15s;
  }
  .btn-sm:hover { color: var(--text); border-color: var(--muted); }
  .btn-primary {
    background: var(--cyan); border: 1px solid var(--cyan); color: var(--bg);
    font-family: inherit; font-size: 10px; padding: 4px 14px; border-radius: 4px;
    cursor: pointer; font-weight: 600;
  }
  .btn-primary:hover { opacity: 0.9; }
  .status-dot {
    width: 6px; height: 6px; border-radius: 50%; flex-shrink: 0;
  }
  .status-dot.ok { background: #34d399; }
  .status-dot.none { background: var(--border); }
  .status-dot.error { background: #f87171; }
  .modal-footer {
    display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px;
    padding-top: 12px; border-top: 1px solid var(--border);
  }
```

- [ ] **Step 2: Add gear icon to header**

In the header div, after the clear button, add:

```html
    <button id="btn-settings" class="btn-sm" title="settings">⚙</button>
```

- [ ] **Step 3: Add settings modal HTML**

After the shortcut overlay div, add:

```html
  <div id="settings-overlay" class="modal-overlay">
    <div class="modal-panel">
      <h3>api keys</h3>
      <div id="settings-providers"></div>
      <div class="modal-footer">
        <button class="btn-sm" id="settings-cancel">cancel</button>
        <button class="btn-primary" id="settings-save">save</button>
      </div>
    </div>
  </div>
```

- [ ] **Step 4: Add settings JS**

Add after the shortcut overlay JS:

```javascript
const $settingsOverlay = document.getElementById('settings-overlay');
const $settingsProviders = document.getElementById('settings-providers');
const PROVIDERS = ['openai', 'openrouter', 'groq', 'opencode', 'anthropic'];

document.getElementById('btn-settings').addEventListener('click', async () => {
  await loadSettings();
  $settingsOverlay.classList.add('visible');
});

document.getElementById('settings-cancel').addEventListener('click', () => {
  $settingsOverlay.classList.remove('visible');
});

$settingsOverlay.addEventListener('click', (e) => {
  if (e.target === $settingsOverlay) $settingsOverlay.classList.remove('visible');
});

async function loadSettings() {
  const { invoke } = window.__TAURI__.core;
  const config = await invoke('get_config');
  $settingsProviders.textContent = '';

  for (const name of PROVIDERS) {
    const existing = config.providers[name] || { api_key: '', enabled: false };
    const group = document.createElement('div');
    group.className = 'field-group';

    const label = document.createElement('label');
    label.className = 'field-label';
    label.textContent = name;

    const row = document.createElement('div');
    row.className = 'field-row';

    const dot = document.createElement('span');
    dot.className = 'status-dot ' + (existing.api_key ? 'ok' : 'none');
    dot.dataset.provider = name;

    const input = document.createElement('input');
    input.className = 'field-input';
    input.type = 'password';
    input.placeholder = `${name} API key`;
    input.value = existing.api_key || '';
    input.dataset.provider = name;

    const testBtn = document.createElement('button');
    testBtn.className = 'btn-sm';
    testBtn.textContent = 'test';
    testBtn.addEventListener('click', async () => {
      testBtn.textContent = '...';
      try {
        // Temporarily save so the backend can use the key
        await saveCurrentSettings();
        const models = await invoke('list_models', { providerName: name });
        dot.className = 'status-dot ok';
        testBtn.textContent = `✓ ${models.length}`;
        setTimeout(() => { testBtn.textContent = 'test'; }, 2000);
      } catch (err) {
        dot.className = 'status-dot error';
        testBtn.textContent = 'fail';
        setTimeout(() => { testBtn.textContent = 'test'; }, 2000);
      }
    });

    row.append(dot, input, testBtn);
    group.append(label, row);
    $settingsProviders.appendChild(group);
  }
}

async function saveCurrentSettings() {
  const { invoke } = window.__TAURI__.core;
  const config = await invoke('get_config');
  const inputs = $settingsProviders.querySelectorAll('.field-input');
  inputs.forEach(input => {
    const name = input.dataset.provider;
    const key = input.value.trim();
    config.providers[name] = { api_key: key, enabled: key.length > 0 };
  });
  await invoke('save_config', { config });
}

document.getElementById('settings-save').addEventListener('click', async () => {
  await saveCurrentSettings();
  showToast('settings saved');
  $settingsOverlay.classList.remove('visible');
});
```

- [ ] **Step 5: Add Escape handler for settings**

In the consolidated keydown handler, add before the shortcut overlay Escape check:

```javascript
  if (e.key === 'Escape' && $settingsOverlay.classList.contains('visible')) {
    $settingsOverlay.classList.remove('visible');
    return;
  }
```

- [ ] **Step 6: Verify**

Click gear icon, settings panel opens. Enter a key, click test, verify dot turns green. Save, reopen, key persists. Escape dismisses.

- [ ] **Step 7: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: settings panel for API key management with test button"
```

---

## Chunk 2: Setup Wizard

### Task 2: Wizard — step 1 (preset & provider status)

**Files:**
- Modify: `src/index.html`

- [ ] **Step 1: Add "new debate" button to header**

In the header, after the team-select dropdown, add:

```html
    <button id="btn-new-debate" class="btn-sm">+ debate</button>
```

- [ ] **Step 2: Add wizard HTML shell**

After the settings overlay, add:

```html
  <div id="wizard-overlay" class="modal-overlay">
    <div class="modal-panel" style="min-width:480px">
      <div id="wizard-header" style="display:flex;justify-content:space-between;align-items:center;margin-bottom:14px">
        <h3 id="wizard-title" style="margin:0">new debate</h3>
        <span id="wizard-step-indicator" style="font-size:9px;color:var(--muted)">step 1 of 4</span>
      </div>
      <div id="wizard-body"></div>
      <div class="modal-footer">
        <button class="btn-sm" id="wizard-back" style="display:none">back</button>
        <div class="spacer"></div>
        <button class="btn-sm" id="wizard-cancel">cancel</button>
        <button class="btn-primary" id="wizard-next">next</button>
      </div>
    </div>
  </div>
```

- [ ] **Step 3: Add wizard state and navigation JS**

```javascript
const $wizardOverlay = document.getElementById('wizard-overlay');
const $wizardBody = document.getElementById('wizard-body');
const $wizardTitle = document.getElementById('wizard-title');
const $wizardStep = document.getElementById('wizard-step-indicator');
const $wizardBack = document.getElementById('wizard-back');
const $wizardNext = document.getElementById('wizard-next');

let wizardState = {
  step: 1,
  preset: null,
  teamName: '',
  agents: [],
  topics: '',
  visibility: 'group',
  termination: 'convergence',
  maxRounds: 10,
  convergenceThreshold: 2,
};

document.getElementById('btn-new-debate').addEventListener('click', () => {
  wizardState = {
    step: 1, preset: null, teamName: '', agents: [],
    topics: '', visibility: 'group', termination: 'convergence',
    maxRounds: 10, convergenceThreshold: 2,
  };
  renderWizardStep();
  $wizardOverlay.classList.add('visible');
});

document.getElementById('wizard-cancel').addEventListener('click', () => {
  $wizardOverlay.classList.remove('visible');
});

$wizardOverlay.addEventListener('click', (e) => {
  if (e.target === $wizardOverlay) $wizardOverlay.classList.remove('visible');
});

$wizardBack.addEventListener('click', () => {
  if (wizardState.step > 1) {
    saveCurrentWizardStep();
    wizardState.step--;
    renderWizardStep();
  }
});

$wizardNext.addEventListener('click', async () => {
  saveCurrentWizardStep();
  if (wizardState.step < 4) {
    wizardState.step++;
    renderWizardStep();
  } else {
    await launchDebate();
  }
});

function renderWizardStep() {
  $wizardStep.textContent = `step ${wizardState.step} of 4`;
  $wizardBack.style.display = wizardState.step > 1 ? '' : 'none';
  $wizardNext.textContent = wizardState.step === 4 ? 'start debate' : 'next';

  switch (wizardState.step) {
    case 1: renderStep1(); break;
    case 2: renderStep2(); break;
    case 3: renderStep3(); break;
    case 4: renderStep4(); break;
  }
}

function saveCurrentWizardStep() {
  switch (wizardState.step) {
    case 1: saveStep1(); break;
    case 2: saveStep2(); break;
    case 3: saveStep3(); break;
  }
}
```

- [ ] **Step 4: Implement step 1 — preset selector and team name**

```javascript
let cachedPresets = [];
let cachedRoles = [];

async function renderStep1() {
  const { invoke } = window.__TAURI__.core;
  if (cachedPresets.length === 0) {
    cachedPresets = await invoke('list_debate_presets');
    cachedRoles = await invoke('list_role_presets');
  }
  const config = await invoke('get_config');

  $wizardTitle.textContent = 'new debate';
  $wizardBody.textContent = '';

  // Team name
  const nameGroup = document.createElement('div');
  nameGroup.className = 'field-group';
  const nameLabel = document.createElement('label');
  nameLabel.className = 'field-label';
  nameLabel.textContent = 'debate name';
  const nameInput = document.createElement('input');
  nameInput.className = 'field-input';
  nameInput.id = 'wizard-team-name';
  nameInput.placeholder = 'e.g., rust-vs-go';
  nameInput.value = wizardState.teamName;
  nameGroup.append(nameLabel, nameInput);

  // Preset selector
  const presetGroup = document.createElement('div');
  presetGroup.className = 'field-group';
  const presetLabel = document.createElement('label');
  presetLabel.className = 'field-label';
  presetLabel.textContent = 'preset';
  const presetSelect = document.createElement('select');
  presetSelect.className = 'field-input';
  presetSelect.id = 'wizard-preset';
  presetSelect.style.cursor = 'pointer';

  const customOpt = document.createElement('option');
  customOpt.value = '';
  customOpt.textContent = 'custom';
  presetSelect.appendChild(customOpt);

  cachedPresets.forEach((p, i) => {
    const opt = document.createElement('option');
    opt.value = i;
    opt.textContent = `${p.name} — ${p.description}`;
    presetSelect.appendChild(opt);
  });

  presetSelect.addEventListener('change', () => {
    const idx = presetSelect.value;
    if (idx !== '') {
      const preset = cachedPresets[parseInt(idx)];
      wizardState.preset = preset;
      wizardState.agents = preset.agents.map(a => {
        const role = cachedRoles.find(r => r.name === a.role);
        return {
          name: a.name,
          provider: '',
          model: '',
          role: a.role,
          system_prompt: role ? role.system_prompt : '',
        };
      });
      wizardState.visibility = preset.visibility;
      wizardState.termination = preset.termination;
      wizardState.maxRounds = preset.default_rounds;
    }
  });

  presetGroup.append(presetLabel, presetSelect);

  // Provider status
  const statusGroup = document.createElement('div');
  statusGroup.className = 'field-group';
  const statusLabel = document.createElement('label');
  statusLabel.className = 'field-label';
  statusLabel.textContent = 'configured providers';
  const statusRow = document.createElement('div');
  statusRow.style.cssText = 'display:flex;gap:10px;flex-wrap:wrap;margin-top:4px';

  PROVIDERS.forEach(name => {
    const hasKey = config.providers[name] && config.providers[name].api_key;
    const chip = document.createElement('span');
    chip.style.cssText = `font-size:10px;color:${hasKey ? '#34d399' : 'var(--muted)'}`;
    chip.textContent = `${hasKey ? '●' : '○'} ${name}`;
    statusRow.appendChild(chip);
  });

  statusGroup.append(statusLabel, statusRow);

  $wizardBody.append(nameGroup, presetGroup, statusGroup);
}

function saveStep1() {
  const nameInput = document.getElementById('wizard-team-name');
  if (nameInput) wizardState.teamName = nameInput.value.trim();
}
```

- [ ] **Step 5: Verify**

Click "+ debate" button, wizard step 1 appears. Select a preset, see it populate. Enter a team name. Click next.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: wizard step 1 — preset selector, team name, provider status"
```

---

### Task 3: Wizard — steps 2-4 and launch

**Files:**
- Modify: `src/index.html`

- [ ] **Step 1: Implement step 2 — agent configuration**

```javascript
async function renderStep2() {
  $wizardTitle.textContent = 'configure agents';
  $wizardBody.textContent = '';

  if (wizardState.agents.length === 0) {
    wizardState.agents = [
      { name: 'agent-1', provider: '', model: '', role: '', system_prompt: '' },
      { name: 'agent-2', provider: '', model: '', role: '', system_prompt: '' },
    ];
  }

  const list = document.createElement('div');
  list.id = 'wizard-agent-list';

  wizardState.agents.forEach((agent, i) => renderAgentRow(list, agent, i));

  const addBtn = document.createElement('button');
  addBtn.className = 'btn-sm';
  addBtn.textContent = '+ add agent';
  addBtn.style.marginTop = '8px';
  addBtn.addEventListener('click', () => {
    wizardState.agents.push({
      name: `agent-${wizardState.agents.length + 1}`,
      provider: '', model: '', role: '', system_prompt: '',
    });
    renderStep2();
  });

  $wizardBody.append(list, addBtn);
}

function renderAgentRow(container, agent, idx) {
  const group = document.createElement('div');
  group.className = 'field-group';
  group.style.cssText = 'border:1px solid var(--border);border-radius:4px;padding:8px;margin-bottom:8px';

  const headerRow = document.createElement('div');
  headerRow.style.cssText = 'display:flex;justify-content:space-between;align-items:center;margin-bottom:6px';

  const nameInput = document.createElement('input');
  nameInput.className = 'field-input';
  nameInput.style.width = '120px';
  nameInput.value = agent.name;
  nameInput.dataset.idx = idx;
  nameInput.dataset.field = 'name';

  const removeBtn = document.createElement('button');
  removeBtn.className = 'btn-sm';
  removeBtn.textContent = '×';
  removeBtn.style.cssText = 'padding:2px 6px;font-size:12px';
  removeBtn.addEventListener('click', () => {
    if (wizardState.agents.length <= 2) { showToast('minimum 2 agents'); return; }
    wizardState.agents.splice(idx, 1);
    renderStep2();
  });

  headerRow.append(nameInput, removeBtn);

  // Provider + model row
  const pmRow = document.createElement('div');
  pmRow.className = 'field-row';
  pmRow.style.marginBottom = '4px';

  const providerSelect = document.createElement('select');
  providerSelect.className = 'field-input';
  providerSelect.style.width = '120px';
  providerSelect.dataset.idx = idx;
  providerSelect.dataset.field = 'provider';
  const defaultOpt = document.createElement('option');
  defaultOpt.value = '';
  defaultOpt.textContent = 'provider...';
  providerSelect.appendChild(defaultOpt);
  PROVIDERS.forEach(p => {
    const opt = document.createElement('option');
    opt.value = p;
    opt.textContent = p;
    if (agent.provider === p) opt.selected = true;
    providerSelect.appendChild(opt);
  });

  const modelInput = document.createElement('input');
  modelInput.className = 'field-input';
  modelInput.placeholder = 'model ID';
  modelInput.value = agent.model;
  modelInput.dataset.idx = idx;
  modelInput.dataset.field = 'model';

  pmRow.append(providerSelect, modelInput);

  // Role selector
  const roleRow = document.createElement('div');
  roleRow.className = 'field-row';
  roleRow.style.marginBottom = '4px';

  const roleSelect = document.createElement('select');
  roleSelect.className = 'field-input';
  roleSelect.dataset.idx = idx;
  roleSelect.dataset.field = 'role';
  const customRoleOpt = document.createElement('option');
  customRoleOpt.value = 'custom';
  customRoleOpt.textContent = 'custom role';
  roleSelect.appendChild(customRoleOpt);
  cachedRoles.forEach(r => {
    const opt = document.createElement('option');
    opt.value = r.name;
    opt.textContent = `${r.name} — ${r.description}`;
    if (agent.role === r.name) opt.selected = true;
    roleSelect.appendChild(opt);
  });

  roleSelect.addEventListener('change', () => {
    const role = cachedRoles.find(r => r.name === roleSelect.value);
    if (role) {
      const promptEl = group.querySelector('[data-field="system_prompt"]');
      if (promptEl) promptEl.value = role.system_prompt;
      wizardState.agents[idx].system_prompt = role.system_prompt;
      wizardState.agents[idx].role = roleSelect.value;
    }
  });

  roleRow.appendChild(roleSelect);

  // System prompt
  const promptArea = document.createElement('textarea');
  promptArea.className = 'field-input';
  promptArea.style.cssText = 'width:100%;min-height:50px;resize:vertical';
  promptArea.value = agent.system_prompt;
  promptArea.placeholder = 'system prompt...';
  promptArea.dataset.idx = idx;
  promptArea.dataset.field = 'system_prompt';

  group.append(headerRow, pmRow, roleRow, promptArea);
  container.appendChild(group);
}

function saveStep2() {
  const list = document.getElementById('wizard-agent-list');
  if (!list) return;
  list.querySelectorAll('[data-idx]').forEach(el => {
    const idx = parseInt(el.dataset.idx);
    const field = el.dataset.field;
    if (wizardState.agents[idx] && field) {
      wizardState.agents[idx][field] = el.value;
    }
  });
}
```

- [ ] **Step 2: Implement step 3 — rules**

```javascript
function renderStep3() {
  $wizardTitle.textContent = 'debate rules';
  $wizardBody.textContent = '';

  // Topics
  const topicGroup = document.createElement('div');
  topicGroup.className = 'field-group';
  const topicLabel = document.createElement('label');
  topicLabel.className = 'field-label';
  topicLabel.textContent = 'topics (one per line)';
  const topicArea = document.createElement('textarea');
  topicArea.className = 'field-input';
  topicArea.id = 'wizard-topics';
  topicArea.style.cssText = 'width:100%;min-height:60px;resize:vertical';
  topicArea.value = wizardState.topics;
  topicArea.placeholder = 'e.g., should we use rust or go for the backend?';
  topicGroup.append(topicLabel, topicArea);

  // Visibility
  const visGroup = document.createElement('div');
  visGroup.className = 'field-group';
  const visLabel = document.createElement('label');
  visLabel.className = 'field-label';
  visLabel.textContent = 'visibility';
  const visSelect = document.createElement('select');
  visSelect.className = 'field-input';
  visSelect.id = 'wizard-visibility';
  ['group', 'directed'].forEach(v => {
    const opt = document.createElement('option');
    opt.value = v;
    opt.textContent = v === 'group' ? 'group chat (all see all)' : 'directed (only see messages to you)';
    if (wizardState.visibility === v) opt.selected = true;
    visSelect.appendChild(opt);
  });
  visGroup.append(visLabel, visSelect);

  // Termination
  const termGroup = document.createElement('div');
  termGroup.className = 'field-group';
  const termLabel = document.createElement('label');
  termLabel.className = 'field-label';
  termLabel.textContent = 'termination mode';
  const termSelect = document.createElement('select');
  termSelect.className = 'field-input';
  termSelect.id = 'wizard-termination';
  [
    ['fixed', 'fixed rounds'],
    ['topic', 'topic-based'],
    ['manual', 'manual stop'],
    ['convergence', 'convergence detection'],
  ].forEach(([val, label]) => {
    const opt = document.createElement('option');
    opt.value = val;
    opt.textContent = label;
    if (wizardState.termination === val) opt.selected = true;
    termSelect.appendChild(opt);
  });
  termGroup.append(termLabel, termSelect);

  // Max rounds (shown for fixed and as limit for others)
  const roundGroup = document.createElement('div');
  roundGroup.className = 'field-group';
  const roundLabel = document.createElement('label');
  roundLabel.className = 'field-label';
  roundLabel.textContent = 'max rounds';
  const roundInput = document.createElement('input');
  roundInput.className = 'field-input';
  roundInput.id = 'wizard-max-rounds';
  roundInput.type = 'number';
  roundInput.min = '1';
  roundInput.max = '100';
  roundInput.value = wizardState.maxRounds;
  roundInput.style.width = '80px';
  roundGroup.append(roundLabel, roundInput);

  $wizardBody.append(topicGroup, visGroup, termGroup, roundGroup);
}

function saveStep3() {
  const topics = document.getElementById('wizard-topics');
  if (topics) wizardState.topics = topics.value;
  const vis = document.getElementById('wizard-visibility');
  if (vis) wizardState.visibility = vis.value;
  const term = document.getElementById('wizard-termination');
  if (term) wizardState.termination = term.value;
  const rounds = document.getElementById('wizard-max-rounds');
  if (rounds) wizardState.maxRounds = parseInt(rounds.value) || 10;
}
```

- [ ] **Step 3: Implement step 4 — review and launch**

```javascript
function renderStep4() {
  $wizardTitle.textContent = 'review & launch';
  $wizardBody.textContent = '';

  const summary = document.createElement('div');
  summary.style.cssText = 'font-size:10px;color:var(--text);line-height:1.8';

  const teamLine = document.createElement('div');
  teamLine.textContent = `team: ${wizardState.teamName || '(unnamed)'}`;

  const agentLines = document.createElement('div');
  agentLines.style.margin = '6px 0';
  wizardState.agents.forEach(a => {
    const line = document.createElement('div');
    line.style.color = 'var(--muted)';
    line.textContent = `  ${a.name} — ${a.provider || '?'}/${a.model || '?'} (${a.role || 'custom'})`;
    agentLines.appendChild(line);
  });

  const topicLine = document.createElement('div');
  const topicList = wizardState.topics.split('\n').filter(t => t.trim());
  topicLine.textContent = `topics: ${topicList.length || 'none'}`;

  const rulesLine = document.createElement('div');
  rulesLine.textContent = `visibility: ${wizardState.visibility} · termination: ${wizardState.termination} · max rounds: ${wizardState.maxRounds}`;

  summary.append(teamLine, agentLines, topicLine, rulesLine);

  // Validation warnings
  const warnings = [];
  if (!wizardState.teamName) warnings.push('no team name set');
  wizardState.agents.forEach(a => {
    if (!a.provider) warnings.push(`${a.name}: no provider selected`);
    if (!a.model) warnings.push(`${a.name}: no model selected`);
  });

  if (warnings.length > 0) {
    const warnDiv = document.createElement('div');
    warnDiv.style.cssText = 'margin-top:10px;padding:6px 10px;background:rgba(251,191,36,0.1);border:1px solid rgba(251,191,36,0.3);border-radius:4px;font-size:9px;color:var(--amber)';
    warnings.forEach(w => {
      const line = document.createElement('div');
      line.textContent = `⚠ ${w}`;
      warnDiv.appendChild(line);
    });
    summary.appendChild(warnDiv);
  }

  $wizardBody.appendChild(summary);
}

async function launchDebate() {
  const { invoke } = window.__TAURI__.core;

  if (!wizardState.teamName) {
    showToast('enter a team name');
    return;
  }

  const hasErrors = wizardState.agents.some(a => !a.provider || !a.model);
  if (hasErrors) {
    showToast('all agents need a provider and model');
    return;
  }

  const topicList = wizardState.topics.split('\n').filter(t => t.trim());

  try {
    const debateConfig = {
      team_name: wizardState.teamName,
      agents: wizardState.agents.map(a => ({
        name: a.name,
        provider: a.provider,
        model: a.model,
        system_prompt: a.system_prompt,
      })),
      topics: topicList,
      visibility: wizardState.visibility,
      termination: wizardState.termination,
      max_rounds: wizardState.maxRounds,
      convergence_threshold: wizardState.convergenceThreshold,
    };

    await invoke('create_debate', { config: debateConfig });
    await invoke('start_debate_cmd', { teamName: wizardState.teamName });

    // Switch to the new debate's team
    populateTeams([wizardState.teamName]);
    $teamSelect.value = wizardState.teamName;
    currentFilter = wizardState.teamName;
    applyFilter();

    $wizardOverlay.classList.remove('visible');
    showToast(`debate "${wizardState.teamName}" started`);
  } catch (err) {
    showToast(`error: ${err}`);
  }
}
```

- [ ] **Step 4: Add Escape handler for wizard**

In the keydown handler, add before other Escape checks:

```javascript
  if (e.key === 'Escape' && $wizardOverlay.classList.contains('visible')) {
    $wizardOverlay.classList.remove('visible');
    return;
  }
```

- [ ] **Step 5: Verify**

Full wizard flow: click "+ debate", select preset, configure agents with provider/model, set rules, review, launch. Debate appears in team selector. Messages start flowing in chat.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: setup wizard steps 2-4 with agent config, rules, and launch"
```

---

## Chunk 3: Debate Control Bar

### Task 4: Debate control bar in header

**Files:**
- Modify: `src/index.html`

- [ ] **Step 1: Add control bar CSS**

```css
  #debate-controls {
    display: none; align-items: center; gap: 6px;
    font-size: 10px; color: var(--muted);
  }
  #debate-controls.active { display: flex; }
  #debate-status-dot {
    width: 6px; height: 6px; border-radius: 50%;
    animation: pulse-status 1.5s infinite;
  }
  #debate-status-dot.running { background: #34d399; }
  #debate-status-dot.paused { background: var(--amber); animation: none; }
  #debate-status-dot.stopped { background: var(--muted); animation: none; }
  @keyframes pulse-status { 0%,100% { opacity: 1; } 50% { opacity: 0.5; } }
```

- [ ] **Step 2: Add control bar HTML**

In the header, after the search input and before the spacer:

```html
    <div id="debate-controls">
      <span id="debate-status-dot"></span>
      <span id="debate-status-text">stopped</span>
      <span id="debate-round-text"></span>
      <button class="btn-sm" id="btn-pause" title="pause/resume">⏸</button>
      <button class="btn-sm" id="btn-stop" title="stop">⏹</button>
    </div>
```

- [ ] **Step 3: Add control bar JS**

```javascript
const $debateControls = document.getElementById('debate-controls');
const $debateStatusDot = document.getElementById('debate-status-dot');
const $debateStatusText = document.getElementById('debate-status-text');
const $debateRoundText = document.getElementById('debate-round-text');
let activeDebateTeam = null;

// Listen for debate status events
async function initDebateControls() {
  const { listen } = window.__TAURI__.event;

  await listen('debate-status', ({ payload }) => {
    activeDebateTeam = payload.team;
    $debateControls.classList.add('active');
    $debateStatusDot.className = 'status-dot ' + payload.status;
    // Also set id-based class for the pulse animation
    $debateStatusDot.id = 'debate-status-dot';
    $debateStatusDot.classList.add(payload.status);
    $debateStatusText.textContent = payload.status;
    $debateRoundText.textContent = payload.round > 0 ? `· round ${payload.round}` : '';

    if (payload.status === 'stopped' || payload.status === 'converged' || payload.status === 'error') {
      document.getElementById('btn-pause').textContent = '⏸';
    }
  });
}

document.getElementById('btn-pause').addEventListener('click', async () => {
  if (!activeDebateTeam) return;
  const { invoke } = window.__TAURI__.core;
  try {
    await invoke('pause_debate', { teamName: activeDebateTeam });
    const btn = document.getElementById('btn-pause');
    btn.textContent = btn.textContent === '⏸' ? '▶' : '⏸';
  } catch (err) {
    showToast(`error: ${err}`);
  }
});

document.getElementById('btn-stop').addEventListener('click', async () => {
  if (!activeDebateTeam) return;
  const { invoke } = window.__TAURI__.core;
  try {
    await invoke('stop_debate', { teamName: activeDebateTeam });
    showToast('debate stopped');
  } catch (err) {
    showToast(`error: ${err}`);
  }
});

// Hide controls when switching to a non-debate team
$teamSelect.addEventListener('change', () => {
  // Controls only show when debate-status events fire for the selected team
  $debateControls.classList.remove('active');
});
```

- [ ] **Step 4: Call `initDebateControls()` from `init()`**

In the `init()` function, add after the existing `listen` calls:

```javascript
  await initDebateControls();
```

- [ ] **Step 5: Verify**

Start a debate via wizard. Control bar should appear with green pulsing dot, round counter, pause and stop buttons. Pause should toggle to resume. Stop should end the debate.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: debate control bar with pause/resume/stop and status indicator"
```

---

## Chunk 4: Final Integration and Tag

### Task 5: Update shortcut overlay and README

**Files:**
- Modify: `src/index.html` (shortcut overlay entries)
- Modify: `README.md` (document new feature)

- [ ] **Step 1: Update shortcut overlay with new shortcuts**

Add to the shortcut panel HTML (or update dynamically):

```html
      <div class="shortcut-row"><span>new debate</span><span class="shortcut-key">⌘N</span></div>
```

Add `Cmd+N` handler in keydown:

```javascript
  if (e.metaKey && e.key === 'n') {
    e.preventDefault();
    document.getElementById('btn-new-debate').click();
  }
```

- [ ] **Step 2: Update README**

Add a new section to README.md after the existing usage section:

```markdown
## multi-model debates

agora can orchestrate debates between LLM agents from different providers. configure your API keys in settings (gear icon), then click "+ debate" or press ⌘N to create a new debate.

### supported providers

- openai (gpt-4o, etc.)
- openrouter (any model they proxy)
- groq (llama, mixtral, etc.)
- opencode
- anthropic (claude)

### setup

1. open settings (gear icon in header)
2. enter API keys for the providers you want to use
3. click "test" to verify each key
4. click "+ debate" to create a new debate
5. pick a preset or configure agents manually
6. set topics, visibility mode, and termination rules
7. launch and watch the debate in real time
```

- [ ] **Step 3: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html README.md
git commit -m "feat: cmd+n shortcut, update shortcut overlay and README for multi-model debates"
```

---

### Task 6: Full build, verify, and tag

- [ ] **Step 1: Full build**

```bash
cd /Volumes/onn/debate-watch
PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" \
CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp \
cargo tauri build --target aarch64-apple-darwin
```

- [ ] **Step 2: Clear WebKit cache and install**

```bash
rm -rf ~/Library/{WebKit,Caches,"Application Support","Saved Application State"}/dev.notbatman.agora
cp -r /Volumes/onn/.cargo-tmp/aarch64-apple-darwin/release/bundle/macos/agora.app /Applications/
```

- [ ] **Step 3: Smoke test**

- [ ] Settings: open gear, enter key, test, save, reopen — key persists
- [ ] Wizard: click + debate, select preset, configure agents, set rules, launch
- [ ] Debate runs: messages appear in chat with correct from/to routing
- [ ] Control bar: pause/resume/stop work, round counter updates
- [ ] Team selector: debate appears alongside any Claude Code teams
- [ ] Export: Cmd+E exports debate transcript
- [ ] All v2 features still work (chips, code blocks, search, etc.)

- [ ] **Step 4: Version bump and tag**

```bash
# Update version in tauri.conf.json to "0.3.0"
cd /Volumes/onn/debate-watch
git add src-tauri/tauri.conf.json
git commit -m "bump version to 0.3.0"
git tag -a v0.3.0 -m "v0.3.0: multi-model debate orchestration with OpenAI, OpenRouter, Groq, OpenCode, and Anthropic support"
git push origin main --tags
```
