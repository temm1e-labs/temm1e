# 06 — Testing Strategy

**Goal:** Every tier ships with verification that covers unit,
integration, and manual scenarios. Multi-turn CLI self-test is
mandatory per `MEMORY.md` workflow rules.

---

## 1. Test coverage by tier

| Tier | Unit | Integration | Manual | Multi-turn self-test |
|------|------|-------------|--------|---------------------|
| A1 (empty commands) | overlay render | snapshot per overlay kind | open each overlay | yes |
| A2 (/help rewrite) | help content | n/a | eyeball help text | yes |
| A3 (git info) | detect_git_info | n/a | in repo / not in repo / detached HEAD | yes |
| A4 (mouse toggle) | state toggle | n/a | terminal selection works | yes |
| A5 (code block yank) | markdown extraction, clipboard write | n/a | copy + paste across platforms | yes |
| A6 (hint bar) | hint_for_state | n/a | eyeball in each state | yes |
| B1/B2 (enum changes) | Display render | runtime emission | n/a | yes |
| B3 (runtime emission) | helper fns | sequence test | n/a | yes |
| B4 (activity panel) | update_status logic | n/a | eyeball streaming trace | yes |
| B5 (collapsed thinking) | match coverage | n/a | eyeball | yes |
| C1 (plumbing) | cancel_token_presence | existing + plumbing smoke | n/a | yes |
| C2 (provider cancel) | n/a | 5 scenarios | press Esc during thinking | yes |
| C3 (tool cancel) | n/a | 5 scenarios | press Esc during shell, browser | yes |
| C4 (Escape handler) | key routing | n/a | press Esc in multiple states | yes |
| C5 (cancel UI) | render_lines | n/a | eyeball after cancel | yes |
| D1-5 | each has its own unit tests | n/a | eyeball per feature | yes |

---

## 2. Unit test additions

### 2.1 `agent_task_status.rs`

```rust
#[test]
fn display_tool_completed() {
    let phase = AgentTaskPhase::ToolCompleted {
        round: 1,
        tool_name: "shell".to_string(),
        tool_index: 0,
        tool_total: 1,
        duration_ms: 42,
        ok: true,
        result_preview: "hello".to_string(),
    };
    assert_eq!(phase.to_string(), "✓ shell (42ms)");
}

#[test]
fn display_tool_completed_failure() {
    let phase = AgentTaskPhase::ToolCompleted {
        round: 1,
        tool_name: "shell".to_string(),
        tool_index: 0,
        tool_total: 1,
        duration_ms: 100,
        ok: false,
        result_preview: "error".to_string(),
    };
    assert_eq!(phase.to_string(), "✗ shell (100ms)");
}

#[test]
fn display_executing_tool_unchanged() {
    let phase = AgentTaskPhase::ExecutingTool {
        round: 1,
        tool_name: "shell".to_string(),
        tool_index: 0,
        tool_total: 3,
        args_preview: "{}".to_string(),
        started_at_ms: 500,
    };
    assert_eq!(phase.to_string(), "Running shell (1/3, round 1)");
}

#[test]
fn phase_variants_clone_includes_new() {
    let phases = vec![
        // ... all 8 variants including ToolCompleted ...
    ];
    for phase in phases {
        let _cloned = phase.clone();
    }
}
```

### 2.2 `widgets/activity_panel.rs`

```rust
#[test]
fn tool_history_caps_at_5() {
    let mut panel = ActivityPanel::new();
    for i in 0..10 {
        let mut status = AgentTaskStatus::default();
        status.phase = AgentTaskPhase::ExecutingTool {
            round: 1,
            tool_name: format!("tool_{i}"),
            tool_index: 0,
            tool_total: 1,
            args_preview: "{}".to_string(),
            started_at_ms: 0,
        };
        panel.update_status(&status);
    }
    assert_eq!(panel.tool_history.len(), 5);
    assert_eq!(panel.tool_history.front().unwrap().name, "tool_5");
    assert_eq!(panel.tool_history.back().unwrap().name, "tool_9");
}

#[test]
fn tool_completed_updates_matching_in_progress() {
    let mut panel = ActivityPanel::new();
    // Push ExecutingTool
    let mut status = AgentTaskStatus::default();
    status.phase = AgentTaskPhase::ExecutingTool {
        round: 1, tool_name: "shell".into(), tool_index: 0, tool_total: 1,
        args_preview: "{}".into(), started_at_ms: 0,
    };
    panel.update_status(&status);
    assert_eq!(panel.tool_history.len(), 1);
    assert_eq!(panel.tool_history[0].status, ToolEventStatus::InProgress);

    // Push ToolCompleted for same tool
    status.phase = AgentTaskPhase::ToolCompleted {
        round: 1, tool_name: "shell".into(), tool_index: 0, tool_total: 1,
        duration_ms: 42, ok: true, result_preview: "done".into(),
    };
    panel.update_status(&status);
    assert_eq!(panel.tool_history.len(), 1);
    assert_eq!(panel.tool_history[0].status, ToolEventStatus::Success);
    assert_eq!(panel.tool_history[0].duration_ms, Some(42));
}

#[test]
fn interrupted_marks_in_progress_as_cancelled() {
    let mut panel = ActivityPanel::new();
    // Start a tool
    let mut status = AgentTaskStatus::default();
    status.phase = AgentTaskPhase::ExecutingTool { /* ... */ };
    panel.update_status(&status);
    // Fire Interrupted
    status.phase = AgentTaskPhase::Interrupted { round: 1 };
    panel.update_status(&status);
    assert_eq!(panel.tool_history[0].status, ToolEventStatus::Failure);
    assert_eq!(panel.tool_history[0].result_preview, Some("[cancelled]".to_string()));
}
```

### 2.3 `app.rs` — scroll mode and key handling

```rust
#[test]
fn escape_closes_overlay_when_open() {
    let mut state = AppState::new();
    state.overlay = Overlay::Help;
    handle_key(&mut state, key_event(KeyCode::Esc));
    assert_eq!(state.overlay, Overlay::None);
}

#[test]
fn escape_triggers_cancel_when_agent_working() {
    let mut state = AppState::new();
    state.is_agent_working = true;
    handle_key(&mut state, key_event(KeyCode::Esc));
    assert!(state.pending_cancel);
}

#[test]
fn escape_is_noop_when_idle_no_overlay() {
    let mut state = AppState::new();
    handle_key(&mut state, key_event(KeyCode::Esc));
    assert!(!state.pending_cancel);
    assert_eq!(state.overlay, Overlay::None);
}

#[test]
fn ctrl_y_opens_picker_when_blocks_exist() {
    let mut state = AppState::new();
    state.code_blocks.push_back(CodeBlock {
        lang: "rust".into(), text: "fn main() {}".into(), line_count: 1,
    });
    handle_key(&mut state, key_event_with_mod(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(state.overlay, Overlay::CopyPicker);
}

#[test]
fn ctrl_y_shows_message_when_no_blocks() {
    let mut state = AppState::new();
    handle_key(&mut state, key_event_with_mod(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(state.overlay, Overlay::None);
    // Check that a system message was pushed
    assert!(state.message_list.messages.iter().any(|m|
        m.content.iter().any(|line| line.spans.iter().any(|s| s.content.contains("No code blocks")))
    ));
}

#[test]
fn alt_s_toggles_mouse_capture() {
    let mut state = AppState::new();
    assert!(state.mouse_capture_enabled);
    handle_key(&mut state, key_event_with_mod(KeyCode::Char('s'), KeyModifiers::ALT));
    assert!(!state.mouse_capture_enabled);
    assert!(state.needs_mouse_toggle);
}
```

### 2.4 `views/config_panel.rs` — per-overlay rendering

For each `OverlayKind`, create a test that:
1. Builds a `AppState` with representative data
2. Renders into a `TestBackend` buffer
3. Asserts that the buffer contains expected text substrings

```rust
#[test]
fn config_overlay_shows_provider_and_model() {
    let mut state = AppState::new();
    state.current_provider = Some("anthropic".into());
    state.current_model = Some("claude-sonnet-4-6".into());
    let buf = render_to_test_buffer(|buf, area| {
        render_config_overlay(&OverlayKind::Config, &state, area, buf);
    });
    let text = buffer_to_string(&buf);
    assert!(text.contains("anthropic"));
    assert!(text.contains("claude-sonnet-4-6"));
}

#[test]
fn keys_overlay_shows_empty_state() {
    let state = AppState::new();
    let buf = render_to_test_buffer(|buf, area| {
        render_config_overlay(&OverlayKind::Keys, &state, area, buf);
    });
    let text = buffer_to_string(&buf);
    assert!(text.contains("No API keys configured"));
}

#[test]
fn usage_overlay_shows_totals() {
    let mut state = AppState::new();
    state.token_counter.total_input_tokens = 1000;
    state.token_counter.total_output_tokens = 500;
    state.token_counter.total_cost_usd = 0.025;
    let buf = render_to_test_buffer(|buf, area| {
        render_config_overlay(&OverlayKind::Usage, &state, area, buf);
    });
    let text = buffer_to_string(&buf);
    assert!(text.contains("1000"));
    assert!(text.contains("500"));
    assert!(text.contains("$0.0250"));
}
```

### 2.5 `lib.rs` — git detection

```rust
#[test]
fn detect_git_info_in_repo() {
    let info = detect_git_info();
    // This test assumes it runs inside a git repo (CI will)
    assert!(info.is_some());
    let info = info.unwrap();
    assert!(!info.repo_name.is_empty());
    assert!(!info.branch.is_empty());
}

// Not easily tested in CI: "not in a repo" case — would need to chdir
// to a non-repo directory which is fragile. Skip automated, verify manually.
```

### 2.6 `widgets/copy_picker.rs` — clipboard

Clipboard tests require a display server, so gate them behind a feature
flag or skip in headless CI:

```rust
#[test]
#[cfg(not(target_os = "linux"))]  // skip on headless Linux CI
fn clipboard_roundtrip() {
    copy_to_clipboard("hello").expect("write");
    let mut cb = arboard::Clipboard::new().unwrap();
    let text = cb.get_text().unwrap();
    assert_eq!(text, "hello");
}

#[test]
fn osc52_encoding() {
    // Pure function, always testable
    let text = "hello";
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    assert_eq!(encoded, "aGVsbG8=");
}
```

---

## 3. Integration tests

### 3.1 Runtime observability (B3)

`crates/temm1e-agent/tests/runtime_integration.rs` additions:

```rust
#[tokio::test]
async fn emits_executing_and_completed_phases() {
    let (runtime, mut status_rx) = build_test_runtime_with_mock_tool().await;
    let mut phases = Vec::new();
    let phase_collector = tokio::spawn(async move {
        while status_rx.changed().await.is_ok() {
            let phase = status_rx.borrow().phase.clone();
            phases.push(phase);
            if matches!(phases.last(), Some(AgentTaskPhase::Done)) {
                break;
            }
        }
        phases
    });

    runtime.process_message(/* ... */).await.unwrap();
    let phases = phase_collector.await.unwrap();

    // Assert the sequence contains ExecutingTool before ToolCompleted
    assert!(phases.iter().any(|p| matches!(p, AgentTaskPhase::ExecutingTool { .. })));
    assert!(phases.iter().any(|p| matches!(p, AgentTaskPhase::ToolCompleted { .. })));
    // And that ExecutingTool comes before ToolCompleted
    // ... exact sequence assertion ...
}

#[tokio::test]
async fn tool_completed_carries_duration_and_ok() {
    // ... similar pattern, extract ToolCompleted fields and assert non-zero duration, ok == true
}

#[tokio::test]
async fn tool_failure_emits_ok_false() {
    // ... mock a failing tool, assert ToolCompleted { ok: false }
}
```

### 3.2 Cancellation scenarios (C2, C3)

```rust
#[tokio::test]
async fn cancel_during_provider_call_emits_interrupted() {
    let (runtime, status_rx) = build_test_runtime_with_slow_provider(Duration::from_secs(10)).await;
    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Cancel after a short delay
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token_clone.cancel();
    });

    let start = Instant::now();
    let result = runtime.process_message(
        &test_msg(),
        &mut SessionContext::new_for_test(),
        None, None, None,
        Some(/* status_tx */),
        Some(token),
    ).await.unwrap();
    let elapsed = start.elapsed();

    // Should return within ~200ms, not 10s
    assert!(elapsed < Duration::from_millis(500));
    assert!(result.0.text.contains("cancelled"));

    // Verify Interrupted was emitted
    assert!(matches!(status_rx.borrow().phase, AgentTaskPhase::Interrupted { round: 1 }));
}

#[tokio::test]
async fn cancel_during_shell_tool_kills_process() {
    // Mock runtime with a real shell tool executing `sleep 30`
    // Cancel after 200ms
    // Verify process exited (ps aux check OR duration assertion)
    // Expected: process dead within 1s
}

#[tokio::test]
async fn cancel_during_second_round_preserves_first_round() {
    // Multi-round setup: round 1 uses tool, round 2 is another provider call
    // Cancel during round 2's provider call
    // Assert: Interrupted { round: 2 }
    // Assert: 1 tool executed (from round 1)
}

#[tokio::test]
async fn double_cancel_is_idempotent() {
    let token = CancellationToken::new();
    token.cancel();
    token.cancel();  // should not panic
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn cancel_before_any_work_still_emits_interrupted() {
    // Cancel immediately before first provider call
    // Verify Interrupted emitted, 0 rounds completed, 0 tools
}

#[tokio::test]
async fn cancel_then_new_message_processes_cleanly() {
    // Turn 1: send message, cancel mid-way
    // Turn 2: send new message
    // Assert: turn 2 runs without issues, no residual cancellation state
}

#[tokio::test]
async fn input_tokens_counted_for_cancelled_round() {
    // Mock provider that reports usage at message_start SSE
    // Cancel mid-stream
    // Assert: BudgetTracker reflects the input_tokens
}

#[tokio::test]
async fn rounds_completed_not_incremented_on_cancel() {
    // Cancel mid-round
    // Assert: status.rounds_completed stays at 0 (or the value before cancel)
}

#[tokio::test]
async fn no_cancel_token_falls_back_to_normal_flow() {
    // Pass None for cancel
    // Assert: normal completion, no Interrupted phase
}

#[tokio::test]
async fn legacy_callers_with_none_still_work() {
    // Explicitly pass None for cancel_token in the new path
    // This ensures we don't break the CLI chat path, gateway, etc.
}
```

### 3.3 Browser tool cancellation (manual test documented)

Not easily automated in CI. Document as a manual verification:

```
Manual: browser tool cancellation
1. Start TUI: cargo run --features tui -- tui
2. Ask Tem to navigate to a slow page: "Open browser and go to https://httpbin.org/delay/30"
3. Wait for "Running browser (...)" indicator
4. Press Escape
5. Observe: "⊗ cancelled" appears in activity panel
6. In another shell: lsof | grep chromium
7. Verify: no lingering CDP connection sockets
8. Send a new message to Tem: "hello"
9. Verify: Tem responds normally, browser pool recovered
```

---

## 4. Multi-turn CLI self-test (MANDATORY)

Per `MEMORY.md`, after all compilation gates pass, run a 10-turn live
conversation test adapted for TUI changes.

### 4.1 CLI chat test (existing pattern from memory)

Run the existing 10-turn protocol from `MEMORY.md` to verify no
regression in baseline message handling:

```bash
# From MEMORY.md — 10-turn CLI chat test
bash /tmp/skyclaw_10turns.sh
```

**Assertions:**
- All 10 turns get responses
- Turn 6 recalls Turn 1 (memory)
- Cost accumulates
- Zero errors in `/tmp/temm1e.log`
- No panics

### 4.2 TUI self-test script (NEW for v4.8.0)

Since we can't easily script the TUI, self-test happens manually via a
scripted checklist:

```
v4.8.0 TUI manual self-test checklist
-------------------------------------

Setup:
[ ] cargo build --release --features tui
[ ] rm -f ~/.temm1e/memory.db
[ ] source /tmp/temm1e_env.sh
[ ] ./target/release/temm1e --features tui tui

Tier A1 — Empty commands fixed:
[ ] Type /config → panel shows provider, model, mode, terminal size
[ ] Type /keys → panel shows "No keys configured" (clean session)
[ ] Type /usage → panel shows zero tokens (fresh)
[ ] Type /status → panel shows "idle"
[ ] Type /model → panel shows list of models with active one highlighted
[ ] All overlays close with Esc

Tier A2 — /help:
[ ] Type /help → content is current
[ ] No /compact in the list

Tier A3 — Git info:
[ ] Status bar right side shows "▣ skyclaw · tui-enhancement"
[ ] Checkout another branch in another terminal → within 5s, status bar updates

Tier A4 — Mouse toggle:
[ ] Alt+S → hint bar shows "SELECT MODE"
[ ] Select text with mouse → terminal's native selection works
[ ] Alt+S again → resume normal mode
[ ] Scroll wheel works again

Tier A5 — Code block yank:
[ ] Ask Tem: "Show me a rust function that reverses a string"
[ ] Ctrl+Y → picker overlay appears
[ ] Press 1 → "Copied block 1 (rust, N lines)" toast
[ ] Paste in another app → code is there

Tier A6 — Hint bar:
[ ] Idle → shows idle hints
[ ] Submit a message → hints change to "Esc cancel · ..."
[ ] Open /config → hints change to "Esc close"

Tier B — Observability:
[ ] Ask Tem: "run ls -la in the current directory"
[ ] Activity panel (Ctrl+O) shows: ▸ shell { ... } then ✓ shell → N files
[ ] Collapsed thinking line shows tool progress live
[ ] Duration is visible per tool

Tier C — Escape cancel:
[ ] Ask Tem: "run sleep 30"
[ ] Wait 2s for shell to start
[ ] Press Esc
[ ] Within 1s: "⊗ cancelled" appears
[ ] Response shows "[cancelled by user ...]"
[ ] Shell process is gone (ps aux | grep sleep)
[ ] Send new message → works normally

[ ] Ask Tem something that takes multiple rounds of thinking
[ ] Press Esc mid-thinking (during provider call)
[ ] Cancellation works cleanly

Tier D (selected):
[ ] State indicator in status bar reflects current phase
[ ] Context meter updates as conversation grows
[ ] /tools opens history overlay

Polish:
[ ] No panics
[ ] No stack traces
[ ] No silent failures
[ ] cargo clippy clean
[ ] cargo fmt clean
[ ] cargo test workspace clean
```

---

## 5. Cross-platform verification

Ideally run on three platforms before release. If only one is available,
run the full checklist on that platform and document any
platform-specific concerns for follow-up.

**macOS:**
- Full checklist
- Clipboard: native arboard
- Mouse toggle: iTerm2, Terminal.app

**Windows:**
- Full checklist (via Windows Terminal + WSL2, or native Windows build)
- Clipboard: native arboard
- Shell tool cancellation: TerminateProcess behavior

**Linux (Ubuntu):**
- Full checklist on X11
- Clipboard: arboard with X11 feature
- Headless SSH: verify OSC 52 fallback

---

## 6. CI additions

`.github/workflows/ci.yml` (or equivalent) should ensure:

1. `cargo check --workspace --all-features` passes
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
3. `cargo fmt --all -- --check` passes
4. `cargo test --workspace` passes
5. Optional: `cargo build --release --features tui` succeeds (catches feature-flag issues)

No new CI steps required; existing gates are sufficient.

---

## 7. Release verification (per `docs/RELEASE_PROTOCOL.md`)

Before pushing v4.8.0:

- [ ] `Cargo.toml` workspace version = `4.8.0`
- [ ] `README.md` badge shows v4.8.0
- [ ] `README.md` hero line mentions v4.8.0 highlights
- [ ] `README.md` metrics table updated if test count changed
- [ ] `README.md` release timeline entry for v4.8.0
- [ ] `CLAUDE.md` crate count unchanged (still 24)
- [ ] `CHANGELOG.md` (or inline README changelog) has v4.8.0 entry
- [ ] All compilation gates green
- [ ] Multi-turn CLI self-test passed
- [ ] TUI manual self-test checklist (above) completed
- [ ] Zero-risk report for Tier C has been re-read and all mitigations confirmed in code
- [ ] `docs/tui/` documents are up to date (this file + the tier reports)

---

## 8. Rollback plan

If a tier breaks after merge:

- Tiers A1-A6 and D1-D5: revert individual commits (independent)
- Tier B (B1-B5): revert as a group (enum change cascades)
- Tier C (C1-C5): revert as a group, though C1 alone is behavior-neutral

The commit ordering in `05-implementation-spec.md` ensures any single
tier can be reverted without dragging others.

---

## 9. Post-release monitoring

After v4.8.0 ships:

- Watch `/tmp/temm1e.log` on production instances for new panic signatures
- Monitor GitHub issues for Escape-cancel reports
- Track any `lsof` anomalies suggesting browser connection leaks
- Review user feedback on the streaming tool trace — is it too noisy? too quiet?

If issues surface, follow the protocol: investigate → reproduce → fix
in a patch release (v4.8.1).
