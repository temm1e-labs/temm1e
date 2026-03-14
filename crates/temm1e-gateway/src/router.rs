//! Message router — routes inbound messages from any channel through
//! the agent runtime and returns the outbound reply.

use temm1e_agent::AgentRuntime;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{InboundMessage, OutboundMessage, TurnUsage};
use temm1e_core::types::session::SessionContext;
use tracing::info;

/// Route an inbound message through the agent runtime.
///
/// Takes a mutable session so the agent can append to conversation history.
/// Returns the outbound message and per-turn usage metrics.
pub async fn route_message(
    msg: &InboundMessage,
    agent: &AgentRuntime,
    session: &mut SessionContext,
) -> Result<(OutboundMessage, TurnUsage), Temm1eError> {
    info!(
        channel = %msg.channel,
        chat_id = %msg.chat_id,
        user_id = %msg.user_id,
        "Routing message to agent runtime"
    );

    agent
        .process_message(msg, session, None, None, None, None, None)
        .await
}
