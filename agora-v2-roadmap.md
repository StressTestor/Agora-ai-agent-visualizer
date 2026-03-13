# agora v2 roadmap

produced by the synthesizer after 3 rounds of advocate/critic debate + probing questions.

constraints held throughout: zero new rust dependencies, zero new tauri plugins, no framework/bundler, single-file frontend, no second rendering mode.

---

## ship this week

ranked by impact-per-hour, highest first. each item is one claude code session.

### 1. click-to-filter agent chips
- what: click a chip to filter messages where that agent is sender OR recipient. multi-select with OR logic. unselected chips dim to 40% opacity. clear button and team-dropdown change both reset the agent filter.
- why: highest-ROI feature. chips already exist and are purely decorative. this makes them the primary navigation tool. every user with >3 agents will use this immediately.
- complexity: ~45 lines JS. one new `Set` (`selectedAgents`), click handlers on chips, compose with existing search filter in `applySearch()`. filter hierarchy: team (rebuilds DOM) > agent (toggles display) > search (toggles display). no rust changes.
- est. hours: 1

### 2. collapsible code blocks
- what: detect triple-backtick fences in message content, render as `<pre><code>` with `--code-bg` background. collapse blocks >8 lines behind a "show code" toggle. strip language hint line if present (don't use it for highlighting).
- why: agent messages are frequently 50%+ code. right now it's unstyled monospace soup. this is the biggest readability fix possible.
- complexity: ~50 lines JS, ~15 lines CSS. split `renderMd()` on `/^```.*$/m` — odd-indexed segments are code, even are prose. inline markdown only runs on prose segments. no syntax highlighting (killed — no deps).
- est. hours: 1.5

### 3. clipboard export (cmd+e / cmd+shift+e)
- what: cmd+e copies current view as markdown, cmd+shift+e copies as JSON. respects all active filters (team + agent chips + search). toast notification confirms: "copied 47 messages as markdown".
- why: people want to share debate transcripts in github issues, docs, slack. clipboard covers 95% of use cases without a file save dialog (killed tauri-plugin-dialog).
- complexity: ~30 lines JS for export formatting, ~15 lines JS+CSS for reusable `showToast(text, ms)`. markdown format: header with team/count/timestamp, then `**from** → to (HH:MM:SS)` with content in blockquotes. JSON is just `JSON.stringify(filteredMessages, null, 2)`. keyboard-only, no header button.
- est. hours: 1.5

### 4. cmd+/ keyboard shortcut overlay
- what: fixed overlay listing all keyboard shortcuts in a two-column layout (key combo | description). instant show/hide via display toggle. escape or click-outside to dismiss.
- why: we chose keyboard-only export (no header button), so users need a way to discover cmd+e, cmd+k, cmd+/, etc. this is the discoverability backstop.
- complexity: ~15 lines JS, ~20 lines CSS. no animation (user-initiated action, not temporal).
- est. hours: 0.5

### 5. message entrance animations
- what: CSS-only slide-in animation on live new-message events. `opacity: 0 → 1` + `translateY(8px → 0)`, 150ms. does NOT fire on initial load or filter-rerender.
- why: makes the live-watching experience feel alive. tiny effort, noticeable polish.
- complexity: ~10 lines CSS. add `.new` class on append, remove after animation ends. zero rust changes.
- est. hours: 0.25

### 6. agent activity pulse (30s decay)
- what: chip dot glows when agent sends a message, fades over 30 seconds via CSS transition on removing `.active` class. 5-second JS interval checks last-message timestamps.
- why: at a glance you see who's actively contributing vs. who's gone quiet. useful during live debates.
- complexity: ~20 lines JS, ~10 lines CSS. track `lastMessageTime` per agent, interval toggles `.active` class.
- est. hours: 0.5

### 7. footer bar chart (agent distribution)
- what: 3px tall flex bar in the footer showing per-agent message proportion using colored segments with `flex-grow` set to each agent's count. sits right of existing msg-count and filter-status spans.
- why: answers "who's dominating the debate?" in peripheral vision. if one agent's segment is 80% of the bar, something's off. costs zero vertical space. note: this is aggregate distribution only — it does NOT replace temporal/relational pattern visualization (that's a v3 conversation based on user feedback).
- complexity: ~20 lines JS, ~15 lines CSS. rebuild on every new message.
- est. hours: 0.5

### 8. /agora companion skill
- what: user-invoked skill for claude code. user types `/agora`, skill lists available teams from `~/.claude/teams/`, user picks one, skill runs `open /Applications/agora.app --args --team <name>`.
- why: bridges the gap between "running a team" and "watching the team." currently you have to manually open the app and pick a team. this makes it one command.
- complexity: shell script + manifest file in `skill/` directory at repo root. README tells users to symlink to their skills directory. no auto-detection (killed — underspecified trigger). no custom URL scheme (killed — --team flag already works).
- est. hours: 0.5
- note: ships on a parallel track. does not gate the v0.2.0 tag.

---

## ship later

ranked by impact-per-hour.

### 1. copy button on code blocks (week 2)
- what: small button positioned `absolute; top: 4px; right: 4px` inside `<pre>` blocks. `navigator.clipboard.writeText(pre.textContent)` on click. `stopPropagation()` to not conflict with click-to-expand. button text toggles to "copied" for 1.5s, reuses `showToast()` pattern.
- why: natural follow-up to code blocks. ships after code blocks have been tested for a day.
- complexity: ~12 lines JS, ~8 lines CSS.
- est. hours: 0.25

### 2. agent adjacency heatmap (v3, user-feedback-driven)
- what: small matrix showing which agents talk to which, with color intensity showing message volume. answers "what was the conversation structure?" without a full swimlane view.
- why: if users want pattern visualization beyond the linear chat, this is the right primitive — compact, no second rendering mode, answers the relational question swimlanes would have answered.
- complexity: TBD. only build this if users actually ask for it.
- est. hours: TBD

---

## killed

features that were proposed and rejected, with reasons.

### 1. swimlane view
- proposed: vertical lanes per agent showing message flow with arrows between senders/recipients. full temporal + relational visualization.
- killed because: 200+ lines of new frontend code, introduces a second rendering mode (every future feature must support both), arrow drawing is a mini rendering engine, breaks down visually beyond 5 agents (131px columns). the linear chat already shows from→to routing. replaced by footer bar chart for aggregate distribution. if temporal pattern visualization is needed later, an adjacency heatmap is the better primitive.

### 2. message count badges on chips
- proposed: show message count next to each agent name in the chip bar, e.g. "advocate (47)".
- killed because: raw counts without context aren't actionable. "advocate (47)" tells you nothing — is that a lot? compared to what? the footer bar chart communicates relative proportion better.

### 3. syntax highlighting in code blocks
- proposed: highlight code in fenced blocks using a tokenizer or library.
- killed because: zero-dependency app. highlight.js is too heavy, a custom tokenizer is maintenance burden. the `<pre><code>` styling with `--code-bg` background is sufficient for a chat viewer.

### 4. file save dialog (tauri-plugin-dialog)
- proposed: cmd+s opens a native save dialog to export the chat as a file.
- killed because: adds a new cargo dependency + tauri plugin for saving a text file. clipboard export covers 95% of use cases. users who need a file can paste from clipboard.

### 5. custom URL scheme (agora://)
- proposed: register `agora://team/deliberation` so the companion skill can deep-link into the app.
- killed because: requires tauri-plugin-deep-link, Info.plist registration, and handler code in main.rs. the `--team` CLI flag already works and the companion skill uses `open --args` to pass it.

### 6. auto-detect team creation in companion skill
- proposed: skill auto-detects when a claude code team is created and prompts user to launch agora.
- killed because: "the tricky part is detecting the right moment" was a red flag during debate. underspecified trigger = underspecified implementation. user-invoked `/agora` is fully specified and predictable.

---

## implementation notes

- total new code: ~280 lines JS/CSS across 8 week-1 features
- all week-1 features are pure frontend except the companion skill (shell script)
- recommended session order: chips → code blocks → export+toast → shortcut overlay → animations → pulse → bar chart → skill
- the first 4 features (chips, code blocks, export, shortcut overlay) deliver 80% of the user value. if time runs short, ship those and defer the rest.
- version bump: tag as v0.2.0 after week-1 features land
