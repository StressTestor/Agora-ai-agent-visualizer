# agora v2 implementation plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship 8 features to index.html (+ 1 companion skill) that transform Agora from a passive chat viewer into an interactive analysis tool.

**Architecture:** All 8 features are pure frontend additions to the single-file `src/index.html`. No Rust changes, no new dependencies, no new Tauri plugins. The companion skill is a separate shell script. Features compose through a filter hierarchy: team (rebuilds DOM) → agent chips (toggles display) → search (toggles display). A shared `showToast()` utility is introduced in task 3 and reused by later features.

**Tech Stack:** Vanilla JS, CSS, Tauri 2 event bridge (existing)

**File:** `src/index.html` (~550 lines currently, will grow to ~830)

**Build/verify command:** `PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp cargo tauri build --target aarch64-apple-darwin 2>&1 | tail -5`

**Quick dev check:** `PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp cargo tauri dev`

**Post-build cache clear:** `rm -rf ~/Library/{WebKit,Caches,"Application Support","Saved Application State"}/dev.notbatman.agora`

---

## Chunk 1: Interactive Filtering

### Task 1: Click-to-filter agent chips

**Files:**
- Modify: `src/index.html` (CSS ~line 89-95, JS ~line 247-298, ~line 404-426, ~line 459-472, ~line 506-521)

**Context:** Chips currently exist as read-only badges showing which agents are in the conversation. This task makes them clickable filters. The filter hierarchy is: team dropdown rebuilds the entire DOM, agent chip filter toggles `display:none` on existing message nodes, search filter also toggles `display:none`. Agent and search filters compose — a message must pass both to be visible.

- [ ] **Step 1: Add CSS for chip states**

After the `.chip-dot` rule (~line 95), add:

```css
  .chip { cursor: pointer; transition: opacity 0.15s; user-select: none; }
  .chip.dimmed { opacity: 0.4; }
```

Note: `.chip` already has `cursor: default` on line 93 — change it to `cursor: pointer`.

- [ ] **Step 2: Add `selectedAgents` state and `data-agent` attribute**

In the State section (~line 247), add:

```javascript
const selectedAgents = new Set();
```

In `ensureChip()` (~line 280), add `data-agent` attribute and click handler to the chip element:

```javascript
  chip.dataset.agent = name;
  chip.addEventListener('click', () => toggleAgentFilter(name));
```

- [ ] **Step 3: Implement `toggleAgentFilter()` and `applyAgentFilter()`**

Add after the `ensureChip` function:

```javascript
function toggleAgentFilter(name) {
  if (selectedAgents.has(name)) {
    selectedAgents.delete(name);
  } else {
    selectedAgents.add(name);
  }
  // Update chip visual state
  $chips.querySelectorAll('.chip').forEach(c => {
    if (selectedAgents.size === 0) {
      c.classList.remove('dimmed');
    } else {
      c.classList.toggle('dimmed', !selectedAgents.has(c.dataset.agent));
    }
  });
  applyAgentFilter();
}

function applyAgentFilter() {
  const msgs = $chat.querySelectorAll('.msg');
  const q = $search.value.toLowerCase();
  let visible = 0;
  msgs.forEach(el => {
    const from = el.querySelector('.msg-route').textContent;
    const to = el.querySelector('.msg-target').textContent;
    const agentMatch = selectedAgents.size === 0 ||
      selectedAgents.has(from) || selectedAgents.has(to);
    const searchMatch = !q || el.querySelector('.msg-body').textContent.toLowerCase().includes(q);
    el.style.display = (agentMatch && searchMatch) ? '' : 'none';
    if (agentMatch && searchMatch) visible++;
  });
  const total = msgs.length;
  $msgCount.textContent = (q || selectedAgents.size > 0)
    ? `${visible} of ${total} messages`
    : `${total} message${total !== 1 ? 's' : ''}`;
}
```

- [ ] **Step 4: Update `applySearch()` to compose with agent filter**

Replace the existing `applySearch()` function:

```javascript
function applySearch() {
  applyAgentFilter(); // reuses the same logic — both filters compose
}
```

- [ ] **Step 5: Reset agent filter on team change and clear**

In `applyFilter()` (~line 404), add after `$search.value = '';`:

```javascript
  selectedAgents.clear();
```

In the clear button handler (~line 452), add before `applyFilter()`:

```javascript
  selectedAgents.clear();
```

- [ ] **Step 6: Apply agent filter to live messages**

In the `new-message` listener (~line 506), after appending the node to DOM, add agent filter check:

```javascript
      // Respect active agent filter
      const agentMatch = selectedAgents.size === 0 ||
        selectedAgents.has(msg.from) || selectedAgents.has(msg.to);
      if (!agentMatch) {
        node.style.display = 'none';
      }
```

- [ ] **Step 7: Verify**

Run `cargo tauri dev`. Click chips — messages should filter. Click again to deselect. Multi-select should OR. Team dropdown change should reset chip selection. Search should compose with chip filter.

- [ ] **Step 8: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: click-to-filter agent chips with OR logic and search composition"
```

---

## Chunk 2: Code Block Rendering

### Task 2: Collapsible code blocks

**Files:**
- Modify: `src/index.html` (CSS after `.msg-body em` ~line 132, JS `renderMd()` ~line 228-236)

**Context:** Agent messages frequently contain triple-backtick fenced code. Currently `renderMd()` treats everything as prose, so code blocks render as unstyled monospace with inline markdown applied incorrectly. This task splits content on fence boundaries, renders code segments as `<pre><code>`, and collapses blocks >8 lines. Note: `renderMd()` already escapes all input via `esc()` before any HTML construction, so the innerHTML usage remains XSS-safe.

- [ ] **Step 1: Add CSS for code blocks**

After `.msg-body em` (~line 132), add:

```css
  .msg-body pre {
    background: var(--code-bg); border-radius: 4px; padding: 8px 10px;
    margin: 4px 0; overflow-x: auto; font-size: 10px; line-height: 1.4;
    max-height: none; position: relative;
  }
  .msg-body pre.collapsed { max-height: 140px; overflow: hidden; }
  .msg-body pre.collapsed::after {
    content: ''; position: absolute; bottom: 0; left: 0; right: 0; height: 30px;
    background: linear-gradient(transparent, var(--code-bg));
  }
  .code-toggle {
    display: block; background: none; border: 1px solid var(--border);
    color: var(--muted); font-family: inherit; font-size: 9px;
    padding: 1px 8px; border-radius: 3px; cursor: pointer;
    margin: 2px 0 4px 0;
  }
  .code-toggle:hover { color: var(--text); border-color: var(--muted); }
```

- [ ] **Step 2: Replace `renderMd()` with fence-aware version**

Replace the existing `renderMd()` function. The function pre-escapes all input via `esc()` before constructing any HTML, maintaining the existing XSS-safe pattern:

```javascript
function renderMd(text) {
  const escaped = esc(text);
  // Split on triple-backtick fences (with optional language hint)
  const parts = escaped.split(/^(```.*)$/m);
  let inCode = false;
  let html = '';
  let codeLineCount = 0;

  for (let i = 0; i < parts.length; i++) {
    const part = parts[i];
    if (part.startsWith('```')) {
      if (!inCode) {
        // Opening fence — count lines in next segment
        const nextPart = parts[i + 1] || '';
        codeLineCount = nextPart.split('\n').filter(l => l.trim()).length;
        const cls = codeLineCount > 8 ? 'code-block collapsed' : 'code-block';
        html += `<pre class="${cls}"><code>`;
        inCode = true;
      } else {
        // Closing fence
        html += '</code></pre>';
        if (codeLineCount > 8) {
          html += '<button class="code-toggle">show code</button>';
        }
        inCode = false;
        codeLineCount = 0;
      }
      continue;
    }
    if (inCode) {
      html += part.replace(/^\n/, '');
    } else {
      html += part
        .replace(/`([^`]+)`/g, '<code>$1</code>')
        .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
        .replace(/__(.+?)__/g, '<strong>$1</strong>')
        .replace(/\*(.+?)\*/g, '<em>$1</em>')
        .replace(/\n/g, '<br>');
    }
  }
  if (inCode) html += '</code></pre>';
  return html;
}
```

- [ ] **Step 3: Add click handler for code toggle buttons**

In `buildMessageNode()`, after setting `body.innerHTML`, add event delegation for toggle buttons:

```javascript
  body.querySelectorAll('.code-toggle').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation(); // don't trigger body expand/collapse
      const pre = btn.previousElementSibling;
      pre.classList.toggle('collapsed');
      btn.textContent = pre.classList.contains('collapsed') ? 'show code' : 'hide code';
    });
  });
```

- [ ] **Step 4: Verify**

Run `cargo tauri dev`. Send a test message with triple-backtick code. Verify:
- Code renders in `<pre><code>` with dark background
- Blocks >8 lines are collapsed with gradient fade
- "show code" / "hide code" toggle works
- Inline markdown in prose segments still works
- Inline backtick code in prose still works

- [ ] **Step 5: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: collapsible code blocks with fence detection and 8-line threshold"
```

---

## Chunk 3: Export, Toast, and Shortcut Overlay

### Task 3: Clipboard export (Cmd+E / Cmd+Shift+E) with toast

**Files:**
- Modify: `src/index.html` (CSS for toast, JS for export + toast + keyboard handler)

**Context:** Users want to share debate transcripts. This adds keyboard-only export (no header button) that copies filtered messages to clipboard as markdown or JSON. A reusable `showToast()` function provides confirmation feedback.

- [ ] **Step 1: Add toast CSS**

After the `#scroll-btn.visible` rule (~line 164), add:

```css
  #toast {
    position: fixed; bottom: 40px; left: 50%; transform: translateX(-50%);
    background: var(--surface); border: 1px solid var(--border);
    color: var(--text); font-size: 10px; padding: 4px 12px;
    border-radius: 4px; opacity: 0; transition: opacity 0.2s;
    pointer-events: none; z-index: 20; white-space: nowrap;
  }
  #toast.visible { opacity: 1; }
```

- [ ] **Step 2: Add toast HTML element**

After the `<button id="scroll-btn">` element, add:

```html
  <div id="toast"></div>
```

- [ ] **Step 3: Implement `showToast()`**

Add in JS after the DOM refs section:

```javascript
const $toast = document.getElementById('toast');
let toastTimer = null;
function showToast(text, ms = 2000) {
  $toast.textContent = text;
  $toast.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => $toast.classList.remove('visible'), ms);
}
```

- [ ] **Step 4: Implement `getFilteredMessages()` helper**

Add after `showToast`:

```javascript
function getFilteredMessages() {
  let filtered = currentFilter
    ? messages.filter(m => m.team === currentFilter)
    : [...messages];
  if (selectedAgents.size > 0) {
    filtered = filtered.filter(m =>
      selectedAgents.has(m.from) || selectedAgents.has(m.to));
  }
  const q = $search.value.toLowerCase();
  if (q) {
    filtered = filtered.filter(m => m.content.toLowerCase().includes(q));
  }
  return filtered;
}
```

- [ ] **Step 5: Implement export functions**

Add after `getFilteredMessages`:

```javascript
function exportMarkdown() {
  const filtered = getFilteredMessages();
  if (filtered.length === 0) { showToast('nothing to export'); return; }
  const team = currentFilter || 'all teams';
  const now = new Date().toISOString().slice(0, 19).replace('T', ' ');
  let md = `# agora transcript — ${team}\n`;
  md += `${filtered.length} messages · exported ${now}\n\n---\n\n`;
  for (const m of filtered) {
    md += `**${m.from}** → ${m.to} (${fmtTime(m.timestamp)})\n`;
    md += `> ${m.content.replace(/\n/g, '\n> ')}\n\n`;
  }
  navigator.clipboard.writeText(md).then(() => {
    showToast(`copied ${filtered.length} messages as markdown`);
  });
}

function exportJSON() {
  const filtered = getFilteredMessages();
  if (filtered.length === 0) { showToast('nothing to export'); return; }
  navigator.clipboard.writeText(JSON.stringify(filtered, null, 2)).then(() => {
    showToast(`copied ${filtered.length} messages as JSON`);
  });
}
```

- [ ] **Step 6: Add keyboard shortcuts**

In the existing `keydown` handler (~line 477), add before the closing `});`:

```javascript
  if (e.metaKey && e.key === 'e') {
    e.preventDefault();
    if (e.shiftKey) { exportJSON(); } else { exportMarkdown(); }
  }
```

- [ ] **Step 7: Verify**

Run `cargo tauri dev`. Load some messages. Press Cmd+E — toast should show "copied N messages as markdown". Paste into a text editor — verify markdown format. Press Cmd+Shift+E — verify JSON. Apply a team filter, then export — verify only filtered messages are exported. Same with search and chip filters.

- [ ] **Step 8: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: clipboard export (cmd+e markdown, cmd+shift+e JSON) with toast"
```

---

### Task 4: Cmd+/ keyboard shortcut overlay

**Files:**
- Modify: `src/index.html` (CSS, HTML, JS keyboard handler)

- [ ] **Step 1: Add overlay CSS**

After the `#toast.visible` rule, add:

```css
  #shortcut-overlay {
    display: none; position: fixed; inset: 0; z-index: 30;
    background: rgba(0,0,0,0.6); align-items: center; justify-content: center;
  }
  #shortcut-overlay.visible { display: flex; }
  #shortcut-panel {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px 24px; min-width: 280px;
  }
  #shortcut-panel h3 {
    font-size: 11px; font-weight: 600; margin-bottom: 10px;
    color: var(--text); text-transform: lowercase;
  }
  .shortcut-row {
    display: flex; justify-content: space-between; padding: 3px 0;
    font-size: 10px; color: var(--muted);
  }
  .shortcut-key {
    font-weight: 600; color: var(--cyan); background: var(--code-bg);
    padding: 1px 5px; border-radius: 3px; font-size: 9px;
  }
```

- [ ] **Step 2: Add overlay HTML**

After the `<div id="toast"></div>` element, add:

```html
  <div id="shortcut-overlay">
    <div id="shortcut-panel">
      <h3>keyboard shortcuts</h3>
      <div class="shortcut-row"><span>search</span><span class="shortcut-key">⌘K</span></div>
      <div class="shortcut-row"><span>export as markdown</span><span class="shortcut-key">⌘E</span></div>
      <div class="shortcut-row"><span>export as JSON</span><span class="shortcut-key">⇧⌘E</span></div>
      <div class="shortcut-row"><span>show shortcuts</span><span class="shortcut-key">⌘/</span></div>
      <div class="shortcut-row"><span>close / clear</span><span class="shortcut-key">Esc</span></div>
    </div>
  </div>
```

- [ ] **Step 3: Add toggle logic**

Add in JS after the toast code:

```javascript
const $overlay = document.getElementById('shortcut-overlay');
$overlay.addEventListener('click', (e) => {
  if (e.target === $overlay) $overlay.classList.remove('visible');
});
```

- [ ] **Step 4: Restructure keyboard handler for overlay priority**

Replace the existing `keydown` handler with one that handles overlay Escape first:

```javascript
document.addEventListener('keydown', (e) => {
  // Overlay takes priority
  if (e.key === 'Escape' && $overlay.classList.contains('visible')) {
    $overlay.classList.remove('visible');
    return;
  }
  if (e.metaKey && e.key === '/') {
    e.preventDefault();
    $overlay.classList.toggle('visible');
    return;
  }
  if (e.metaKey && e.key === 'k') {
    e.preventDefault();
    $search.focus();
    $search.select();
  }
  if (e.metaKey && e.key === 'e') {
    e.preventDefault();
    if (e.shiftKey) { exportJSON(); } else { exportMarkdown(); }
  }
  if (e.key === 'Escape' && document.activeElement === $search) {
    $search.value = '';
    $search.dispatchEvent(new Event('input'));
    $search.blur();
  }
});
```

Note: This replaces the keyboard handler added in earlier tasks. The final version consolidates all shortcuts in one handler with correct priority order.

- [ ] **Step 5: Verify**

Cmd+/ shows overlay. Escape dismisses. Click outside dismisses. All listed shortcuts work.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: cmd+/ keyboard shortcut overlay"
```

---

## Chunk 4: Visual Polish

### Task 5: Message entrance animations

**Files:**
- Modify: `src/index.html` (CSS animation, JS in new-message listener)

- [ ] **Step 1: Add CSS animation**

After the `.msg:hover` rule (~line 108), add:

```css
  @keyframes msg-enter { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: translateY(0); } }
  .msg.entering { animation: msg-enter 0.15s ease-out; }
```

- [ ] **Step 2: Add `.entering` class on live messages only**

In the `new-message` listener, after `const node = buildMessageNode(msg);`, add:

```javascript
      node.classList.add('entering');
      node.addEventListener('animationend', () => node.classList.remove('entering'), { once: true });
```

This does NOT apply to messages rendered by `applyFilter()` (initial load / team change).

- [ ] **Step 3: Verify**

Run dev. Watch live messages slide in. Switching teams should NOT animate. Initial load should NOT animate.

- [ ] **Step 4: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: CSS slide-in animation for live messages"
```

---

### Task 6: Agent activity pulse (30s decay)

**Files:**
- Modify: `src/index.html` (CSS for pulse glow, JS for tracking + interval)

- [ ] **Step 1: Add CSS for active chip dot**

After the `.chip.dimmed` rule, add:

```css
  .chip-dot { transition: box-shadow 0.5s ease-out; }
  .chip-dot.active { box-shadow: 0 0 6px 2px currentColor; }
```

- [ ] **Step 2: Add tracking state and dot data attribute**

In the State section, add:

```javascript
const agentLastMsg = {};  // agent name → timestamp ms
```

In `ensureChip()`, add to the dot element:

```javascript
  dot.dataset.agent = name;
```

- [ ] **Step 3: Mark agent active on new message**

In the `new-message` listener, after `messages.push(msg);`, add:

```javascript
    agentLastMsg[msg.from] = Date.now();
    const dot = $chips.querySelector(`.chip-dot[data-agent="${msg.from}"]`);
    if (dot) dot.classList.add('active');
```

- [ ] **Step 4: Add 5-second decay interval**

After the agent tracking state, add:

```javascript
setInterval(() => {
  const now = Date.now();
  $chips.querySelectorAll('.chip-dot').forEach(dot => {
    const agent = dot.dataset.agent;
    const last = agentLastMsg[agent] || 0;
    if (now - last > 30000) dot.classList.remove('active');
  });
}, 5000);
```

- [ ] **Step 5: Verify**

Watch a live debate. Agent dots should glow on message send and fade after 30s of inactivity.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: agent activity pulse with 30s decay on chip dots"
```

---

### Task 7: Footer bar chart (agent distribution)

**Files:**
- Modify: `src/index.html` (CSS, HTML in footer, JS to rebuild bar)

- [ ] **Step 1: Add bar CSS**

After the `#filter-status` rule, add:

```css
  #agent-bar {
    flex: 1; height: 3px; display: flex; border-radius: 2px;
    overflow: hidden; margin-left: 8px; min-width: 60px;
  }
  #agent-bar span { transition: flex-grow 0.3s; }
```

- [ ] **Step 2: Add bar HTML**

In the footer div, after `<span id="filter-status"></span>`, add:

```html
    <div id="agent-bar"></div>
```

- [ ] **Step 3: Add DOM ref and rebuild function**

Add to DOM refs:

```javascript
const $agentBar = document.getElementById('agent-bar');
```

Add after `updateFooter()`:

```javascript
function updateAgentBar() {
  const visible = currentFilter
    ? messages.filter(m => m.team === currentFilter)
    : messages;
  const counts = {};
  for (const m of visible) {
    counts[m.from] = (counts[m.from] || 0) + 1;
  }
  $agentBar.textContent = '';
  for (const [agent, count] of Object.entries(counts).sort((a, b) => b[1] - a[1])) {
    const seg = document.createElement('span');
    seg.style.flexGrow = count;
    seg.style.background = agentColor(agent);
    seg.title = `${agent}: ${count}`;
    $agentBar.appendChild(seg);
  }
}
```

- [ ] **Step 4: Call `updateAgentBar()` from `updateFooter()`**

At the end of `updateFooter()`, add:

```javascript
  updateAgentBar();
```

- [ ] **Step 5: Verify**

Bar should appear in footer showing colored segments proportional to each agent's message count. Hover segments for tooltip. Changes on team filter switch and new messages.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src/index.html
git commit -m "feat: footer bar chart showing agent message distribution"
```

---

## Chunk 5: Companion Skill

### Task 8: /agora companion skill

**Files:**
- Create: `skill/agora.md` (skill manifest)
- Modify: `README.md` (add skill install instructions)

- [ ] **Step 1: Create skill directory**

```bash
mkdir -p /Volumes/onn/debate-watch/skill
```

- [ ] **Step 2: Write skill manifest**

Create `skill/agora.md` with frontmatter defining the skill name, description, and user_invocable flag. The body contains instructions for the Claude Code agent to list teams and launch the app.

- [ ] **Step 3: Update README with skill install section**

Add a "claude code skill" section after "## usage" explaining how to symlink the skill directory and use `/agora` in a session.

- [ ] **Step 4: Commit**

```bash
cd /Volumes/onn/debate-watch
git add skill/agora.md README.md
git commit -m "feat: /agora companion skill for Claude Code"
```

---

## Chunk 6: Final Verification and Tag

### Task 9: Full build, verify, and tag v0.2.0

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

- [ ] **Step 3: Manual smoke test**

Open `/Applications/agora.app`. Verify:
- [ ] Chips are clickable, filter messages, dim when unselected
- [ ] Code blocks render with dark background, collapse >8 lines
- [ ] Cmd+E copies markdown to clipboard
- [ ] Cmd+Shift+E copies JSON
- [ ] Cmd+/ shows shortcut overlay, Escape dismisses
- [ ] Live messages slide in
- [ ] Chip dots pulse on activity, fade after 30s
- [ ] Footer bar shows agent distribution
- [ ] All existing features still work (search, team filter, clear, scroll-to-bottom)

- [ ] **Step 4: Update version in tauri.conf.json**

Change `"version": "0.1.0"` to `"version": "0.2.0"`.

- [ ] **Step 5: Commit and tag**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/tauri.conf.json
git commit -m "bump version to 0.2.0"
git tag -a v0.2.0 -m "v0.2.0: interactive chips, code blocks, export, shortcuts overlay, animations, activity pulse, bar chart, companion skill"
```

- [ ] **Step 6: Push**

```bash
git push origin main --tags
```
