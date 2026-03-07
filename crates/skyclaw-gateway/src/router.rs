//! Message router — routes inbound messages from any channel through
//! the agent runtime and returns the outbound reply.

use skyclaw_agent::AgentRuntime;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::message::{InboundMessage, OutboundMessage};
use skyclaw_core::types::session::SessionContext;
use tracing::info;

/// Route an inbound message through the agent runtime.
///
/// Takes a mutable session so the agent can append to conversation history.
/// Returns the outbound message to send back to the originating channel.
pub async fn route_message(
    msg: &InboundMessage,
    agent: &AgentRuntime,
    session: &mut SessionContext,
) -> Result<OutboundMessage, SkyclawError> {
    info!(
        channel = %msg.channel,
        chat_id = %msg.chat_id,
        user_id = %msg.user_id,
        "Routing message to agent runtime"
    );

    agent.process_message(msg, session).await
}
