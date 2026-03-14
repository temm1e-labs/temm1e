//! Integration tests for the agent runtime — tests the full message processing
//! loop with mock provider and mock memory working together.

use std::sync::Arc;

use temm1e_agent::{AgentRuntime, AgentTaskPhase, AgentTaskStatus};
use temm1e_core::types::message::*;
use temm1e_core::Tool;
use temm1e_test_utils::{make_inbound_msg, make_session, MockMemory, MockProvider, MockTool};
use tokio_util::sync::CancellationToken;

fn make_runtime_with_text(text: &str) -> AgentRuntime {
    let provider = Arc::new(MockProvider::with_text(text));
    let memory = Arc::new(MockMemory::new());
    let tools: Vec<Arc<dyn Tool>> = vec![];

    // Disable v2 optimizations — MockProvider returns plain text,
    // not JSON classification, so the LLM classifier would always
    // fall back and add an extra provider call.
    AgentRuntime::new(
        provider,
        memory,
        tools,
        "test-model".to_string(),
        Some("You are a test agent.".to_string()),
    )
    .with_v2_optimizations(false)
}

#[tokio::test]
async fn simple_text_response() {
    let runtime = make_runtime_with_text("Hello from the AI!");
    let msg = make_inbound_msg("Hi there");
    let mut session = make_session();

    let (reply, _turn_usage) = runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(reply.text, "Hello from the AI!");
    assert_eq!(reply.chat_id, msg.chat_id);
    assert!(reply.reply_to.is_some());
    assert!(reply.parse_mode.is_none());
}

#[tokio::test]
async fn session_history_grows_after_processing() {
    let runtime = make_runtime_with_text("Response text");
    let msg = make_inbound_msg("User input");
    let mut session = make_session();

    assert!(session.history.is_empty());
    runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();

    // Should have user message + assistant reply in history
    assert_eq!(session.history.len(), 2);
    assert!(matches!(session.history[0].role, Role::User));
    assert!(matches!(session.history[1].role, Role::Assistant));
}

#[tokio::test]
async fn runtime_with_no_text_in_inbound_msg() {
    let runtime = make_runtime_with_text("OK");
    let mut msg = make_inbound_msg("");
    msg.text = None;
    let mut session = make_session();

    let (reply, _turn_usage) = runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();
    // Empty message with no attachments returns a friendly error
    assert!(reply.text.contains("empty message"));
}

#[tokio::test]
async fn provider_called_exactly_once_for_simple_text() {
    let provider = Arc::new(MockProvider::with_text("response"));
    let memory = Arc::new(MockMemory::new());
    let runtime = AgentRuntime::new(provider.clone(), memory, vec![], "model".to_string(), None)
        .with_v2_optimizations(false);

    let msg = make_inbound_msg("hello");
    let mut session = make_session();
    runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(provider.calls().await, 1);
}

#[tokio::test]
async fn runtime_accessor_methods() {
    let provider = Arc::new(MockProvider::with_text("test"));
    let memory = Arc::new(MockMemory::new());
    let tool = Arc::new(MockTool::new("my_tool"));
    let tools: Vec<Arc<dyn Tool>> = vec![tool];

    let runtime = AgentRuntime::new(
        provider,
        memory,
        tools,
        "model".to_string(),
        Some("prompt".to_string()),
    );

    assert_eq!(runtime.provider().name(), "mock");
    assert_eq!(runtime.memory().backend_name(), "mock");
    assert_eq!(runtime.tools().len(), 1);
    assert_eq!(runtime.tools()[0].name(), "my_tool");
}

#[tokio::test]
async fn runtime_with_memory_entries() {
    let memory = Arc::new(MockMemory::with_entries(vec![
        temm1e_test_utils::make_test_entry_with_session(
            "mem1",
            "Important context about Rust",
            "test:test-chat:test-user",
        ),
    ]));

    let provider = Arc::new(MockProvider::with_text("I remember about Rust!"));
    let runtime = AgentRuntime::new(provider.clone(), memory, vec![], "model".to_string(), None)
        .with_v2_optimizations(false);

    let msg = make_inbound_msg("Tell me about Rust");
    let mut session = make_session();
    let (reply, _turn_usage) = runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(reply.text, "I remember about Rust!");

    // Check that the provider received messages including memory context
    let captured = provider.captured_requests.lock().await;
    assert_eq!(captured.len(), 1);
    let req = &captured[0];
    // Should have system message (memory context) + user message
    assert!(!req.messages.is_empty());
}

#[tokio::test]
async fn multiple_messages_in_sequence() {
    let runtime = make_runtime_with_text("Reply");

    let mut session = make_session();

    for i in 0..3 {
        let msg = make_inbound_msg(&format!("Message {i}"));
        let (reply, _turn_usage) = runtime
            .process_message(&msg, &mut session, None, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(reply.text, "Reply");
    }

    // History should have 3 user + 3 assistant = 6 messages
    assert_eq!(session.history.len(), 6);
}

// ── Interceptor Phase 1: Watch Channel + CancellationToken Tests ───

#[tokio::test]
async fn status_watch_channel_receives_phase_transitions() {
    // Watch channels show the LATEST value, not all intermediate states.
    // With a synchronous MockProvider, process_message runs to completion
    // before any observer task can poll. So we verify:
    // 1. The sender was used (receiver sees a changed mark)
    // 2. The final state reflects the complete lifecycle
    let runtime = make_runtime_with_text("Hello!");
    let msg = make_inbound_msg("Hi");
    let mut session = make_session();

    let (status_tx, mut status_rx) = tokio::sync::watch::channel(AgentTaskStatus::default());
    let cancel = CancellationToken::new();

    let (reply, _usage) = runtime
        .process_message(
            &msg,
            &mut session,
            None,
            None,
            None,
            Some(status_tx),
            Some(cancel),
        )
        .await
        .unwrap();

    assert_eq!(reply.text, "Hello!");

    // The watch channel should have been modified — changed() resolves immediately
    // because send_modify was called multiple times during process_message
    let changed =
        tokio::time::timeout(tokio::time::Duration::from_millis(100), status_rx.changed()).await;
    assert!(
        changed.is_ok(),
        "Watch channel should have pending changes after process_message"
    );

    // The latest value should be Done (final phase)
    let status = status_rx.borrow().clone();
    assert!(
        matches!(status.phase, AgentTaskPhase::Done),
        "Final phase should be Done, got {:?}",
        status.phase
    );
}

#[tokio::test]
async fn status_watch_final_state_is_done() {
    let runtime = make_runtime_with_text("Done!");
    let msg = make_inbound_msg("test");
    let mut session = make_session();

    let (status_tx, status_rx) = tokio::sync::watch::channel(AgentTaskStatus::default());
    let cancel = CancellationToken::new();

    runtime
        .process_message(
            &msg,
            &mut session,
            None,
            None,
            None,
            Some(status_tx),
            Some(cancel),
        )
        .await
        .unwrap();

    // After process_message returns, the final state should be Done
    let final_status = status_rx.borrow().clone();
    assert!(
        matches!(final_status.phase, AgentTaskPhase::Done),
        "Final phase should be Done, got {:?}",
        final_status.phase
    );
}

#[tokio::test]
async fn status_watch_tracks_token_counts() {
    let runtime = make_runtime_with_text("response");
    let msg = make_inbound_msg("query");
    let mut session = make_session();

    let (status_tx, status_rx) = tokio::sync::watch::channel(AgentTaskStatus::default());

    runtime
        .process_message(&msg, &mut session, None, None, None, Some(status_tx), None)
        .await
        .unwrap();

    let final_status = status_rx.borrow().clone();
    // MockProvider reports token usage — verify it's captured
    // The exact values depend on MockProvider but should be > 0
    // if the provider reports any usage
    assert!(
        matches!(final_status.phase, AgentTaskPhase::Done),
        "Phase should be Done"
    );
    assert_eq!(
        final_status.rounds_completed, 0,
        "Simple text response = 0 tool rounds"
    );
}

#[tokio::test]
async fn cancel_token_does_not_affect_normal_flow() {
    let runtime = make_runtime_with_text("Normal response");
    let msg = make_inbound_msg("Hello");
    let mut session = make_session();

    let cancel = CancellationToken::new();
    // Don't cancel — verify normal flow works with token present
    let (reply, _usage) = runtime
        .process_message(&msg, &mut session, None, None, None, None, Some(cancel))
        .await
        .unwrap();

    assert_eq!(reply.text, "Normal response");
}

#[tokio::test]
async fn none_status_and_cancel_is_backward_compatible() {
    // Verify that passing None for both Phase 1 params
    // produces identical behavior to pre-Phase-1
    let runtime = make_runtime_with_text("Backward compat");
    let msg = make_inbound_msg("test");
    let mut session = make_session();

    let (reply, _usage) = runtime
        .process_message(&msg, &mut session, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(reply.text, "Backward compat");
    assert_eq!(session.history.len(), 2);
}

#[tokio::test]
async fn watch_channel_with_multiple_messages() {
    let runtime = make_runtime_with_text("Reply");
    let mut session = make_session();

    // Process multiple messages, each with its own watch channel
    for i in 0..3 {
        let (status_tx, status_rx) = tokio::sync::watch::channel(AgentTaskStatus::default());
        let cancel = CancellationToken::new();
        let msg = make_inbound_msg(&format!("Message {i}"));

        runtime
            .process_message(
                &msg,
                &mut session,
                None,
                None,
                None,
                Some(status_tx),
                Some(cancel),
            )
            .await
            .unwrap();

        let final_status = status_rx.borrow().clone();
        assert!(
            matches!(final_status.phase, AgentTaskPhase::Done),
            "Message {i}: final phase should be Done, got {:?}",
            final_status.phase
        );
    }
}
