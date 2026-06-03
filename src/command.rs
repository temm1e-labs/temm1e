//! Canonical slash-command parsing, shared across every chat channel.
//!
//! Historically each channel + entrypoint re-implemented "is this a command?"
//! with divergent `cmd_lower == "/x"` ladders, and none stripped Telegram's
//! group `/cmd@botname` suffix — so the *same* command worked in a DM but was
//! forwarded to the LLM in a group. This module centralizes the rules:
//!
//!   * a command is text whose first non-whitespace token starts with `/`
//!   * the verb is that token, lowercased, with a trailing `@botname` removed
//!   * everything after the first token is the argument string
//!
//! Two entry points are used by the dispatcher in `main.rs`:
//!   * [`is_known_command`] gates routing, so a known command runs as a command
//!     even while a task is in flight (instead of being eaten by the Mission
//!     Control classifier as chat).
//!   * [`normalize_command_text`] rewrites `"/help@Bot …"` → `"/help …"` so the
//!     existing exact-match handlers behave identically in DMs and group chats.

/// Canonical command verbs (without the leading `/`), kept in sync with the
/// `/help` output and the dispatch ladders in `main.rs`. Used only to decide
/// whether a `/token` should be *routed* as a command — being generous here is
/// safe: an unrecognized verb simply falls through to normal processing.
pub const KNOWN_COMMANDS: &[&str] = &[
    "help",
    "addkey",
    "keys",
    "model",
    "removekey",
    "addmodel",
    "listmodels",
    "removemodel",
    "usage",
    "memory",
    "cambium",
    "eigentune",
    "mcp",
    "browser",
    "timelimit",
    "reload",
    "reset",
    "restart",
    "login",
    "vigil",
    "stop",
    "status",
    "queue",
];

/// Parsed view of a slash-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Lowercased verb, without the leading `/` or any `@botname` suffix.
    pub verb: String,
    /// Whatever followed the verb token, trimmed. Empty when there were no args.
    pub args: String,
}

/// Parse `text` as a slash-command. Returns `None` when the first
/// non-whitespace token does not start with `/`, or is just `/`.
pub fn parse_command(text: &str) -> Option<ParsedCommand> {
    let rest = text.trim_start().strip_prefix('/')?;
    // The verb is the first whitespace-delimited token after '/'.
    let (token, args) = match rest.split_once(char::is_whitespace) {
        Some((t, a)) => (t, a.trim()),
        None => (rest, ""),
    };
    // Strip a Telegram group "@botname" suffix from the verb token.
    let verb = token.split('@').next().unwrap_or(token);
    if verb.is_empty() {
        return None;
    }
    Some(ParsedCommand {
        verb: verb.to_ascii_lowercase(),
        args: args.to_string(),
    })
}

/// True if `text` is a recognized slash-command. Used to route commands to the
/// command handler even while a task is running (fixes the "command sometimes
/// runs, sometimes goes to the LLM" bug). `@botname` suffixes are tolerated.
pub fn is_known_command(text: &str) -> bool {
    parse_command(text).is_some_and(|c| KNOWN_COMMANDS.contains(&c.verb.as_str()))
}

/// If `text` is a slash-command whose verb carries a Telegram `@botname` suffix
/// (e.g. `"/help@MyBot arg"`), return the normalized form (`"/help arg"`).
/// Returns `None` when there is nothing to normalize, so callers can skip the
/// reassignment. Leading whitespace is preserved.
pub fn normalize_command_text(text: &str) -> Option<String> {
    let leading_ws_len = text.len() - text.trim_start().len();
    let (leading_ws, body) = text.split_at(leading_ws_len);
    let rest = body.strip_prefix('/')?;
    let (token, after) = match rest.split_once(char::is_whitespace) {
        Some((t, a)) => (t, Some(a)),
        None => (rest, None),
    };
    // Only rewrite when the verb token actually contains an "@".
    let at = token.find('@')?;
    let verb = &token[..at];
    if verb.is_empty() {
        return None;
    }
    Some(match after {
        Some(a) => format!("{leading_ws}/{verb} {a}"),
        None => format!("{leading_ws}/{verb}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_command() {
        let c = parse_command("/help").unwrap();
        assert_eq!(c.verb, "help");
        assert_eq!(c.args, "");
    }

    #[test]
    fn strips_botname_suffix() {
        assert_eq!(parse_command("/help@MyBot").unwrap().verb, "help");
        let c = parse_command("/model@MyBot gpt-4o").unwrap();
        assert_eq!(c.verb, "model");
        assert_eq!(c.args, "gpt-4o");
    }

    #[test]
    fn lowercases_verb_keeps_arg_case() {
        let c = parse_command("/MODEL Gpt-4O").unwrap();
        assert_eq!(c.verb, "model");
        assert_eq!(c.args, "Gpt-4O");
    }

    #[test]
    fn tolerates_leading_whitespace() {
        assert_eq!(parse_command("   /status").unwrap().verb, "status");
    }

    #[test]
    fn non_commands_return_none() {
        assert!(parse_command("hello").is_none());
        assert!(parse_command("").is_none());
        assert!(parse_command("/").is_none());
        assert!(parse_command("   ").is_none());
    }

    #[test]
    fn known_command_detection() {
        assert!(is_known_command("/help"));
        assert!(is_known_command("/help@Bot"));
        assert!(is_known_command("/MODEL gpt-4o"));
        assert!(is_known_command("   /reset"));
        assert!(!is_known_command("/wibble"));
        assert!(!is_known_command("just chatting"));
        // Path-like text is not a command (verb "usr/bin/env" is unknown).
        assert!(!is_known_command("/usr/bin/env"));
    }

    #[test]
    fn normalize_only_when_botname_present() {
        assert_eq!(normalize_command_text("/help@Bot").as_deref(), Some("/help"));
        assert_eq!(
            normalize_command_text("/model@Bot gpt-4o").as_deref(),
            Some("/model gpt-4o")
        );
        assert_eq!(normalize_command_text("/help"), None);
        assert_eq!(normalize_command_text("hello"), None);
        assert_eq!(normalize_command_text("/usr/bin"), None);
        // Leading whitespace is preserved.
        assert_eq!(
            normalize_command_text("  /status@Bot").as_deref(),
            Some("  /status")
        );
    }
}
