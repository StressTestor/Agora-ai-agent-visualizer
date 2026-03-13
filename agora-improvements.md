# agora improvements

ranked by impact-per-hour for a solo dev.

## 1. accurate timestamps (bug fix)
**effort:** 1-2 hours
**impact:** messages show when they were actually sent, not when agora first saw them

right now `now_ms()` is called at ingestion time. if you open agora after a debate already happened, every message gets the same timestamp. fix: `parse_inbox` reads file mtime once at the top. `extract_msg` checks for a JSON `timestamp` field (`val.get("timestamp").and_then(|v| v.as_u64())`). `parse_inbox` resolves each message: JSON timestamp if present, file mtime otherwise, `now_ms()` as absolute last fallback. return type becomes `Vec<(String, String, String, u64)>` — fully resolved, no Option. `scan_inboxes` just uses the timestamp directly. all messages in a multi-message file share the same mtime if no JSON timestamps exist, which is acceptable and still better than ingestion time.

## 2. content search
**effort:** 1 hour
**impact:** find specific arguments in long debates without scrolling through everything

single text input in the header, styled to match the team selector. `placeholder="search..."`. `input` event listener on every keystroke. iterates all `.msg` nodes in `#chat`, toggles `display: none` based on whether `.msg-body` textContent includes the lowercase query. no DOM rebuild, no `applyFilter()` call — just visibility toggling on existing nodes. footer shows "N of M messages" when search is active. empty search = show all. `applyFilter()` (team filter change) clears the search input, so search never fights the team filter. ~20 lines of JS total.

## 3. keyboard shortcuts (Cmd+K / Escape)
**effort:** 30 minutes
**impact:** standard mac shortcuts for search — the entire target audience expects these

single `keydown` listener on `document`. `Cmd+K` (`e.metaKey && e.key === 'k'`): `preventDefault()`, focus search input, select all text. `Escape`: if search input is focused, clear value, dispatch `input` event to reset visibility, blur. if not focused, do nothing. no help overlay, no other shortcuts. two keybindings, both standard mac conventions, both self-documenting.
