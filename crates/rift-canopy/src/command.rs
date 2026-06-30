//! The bottom command palette: slash-command parsing + autocomplete.
//!
//! Commands mirror the `rift` CLI surface; the palette parses `/name [args…]`
//! and offers prefix autocompletion. Pure logic — no terminal — so it is fully
//! testable.

/// The known slash commands (mirror the CLI surface).
pub const COMMANDS: &[&str] = &[
    "scan", "redact", "status", "memory", "verify", "setup", "config", "help", "quit",
];

/// A parsed command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The command name (without the leading `/`).
    pub name: String,
    /// Whitespace-split arguments.
    pub args: Vec<String>,
    /// Whether the name is a recognized command.
    pub recognized: bool,
}

/// Parse a command-line buffer. A leading `/` is optional. Returns `None` for an
/// empty buffer.
#[must_use]
pub fn parse_command(input: &str) -> Option<Command> {
    let trimmed = input.trim().strip_prefix('/').unwrap_or(input.trim());
    let mut parts = trimmed.split_whitespace();
    let name = parts.next()?.to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    let recognized = COMMANDS.contains(&name.as_str());
    Some(Command {
        name,
        args,
        recognized,
    })
}

/// Prefix autocomplete suggestions for the partial command name (sorted).
#[must_use]
pub fn autocomplete(prefix: &str) -> Vec<&'static str> {
    let p = prefix.trim().strip_prefix('/').unwrap_or(prefix.trim());
    let mut out: Vec<&'static str> = COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(p))
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_args() {
        let c = parse_command("/redact --dry-run path/").unwrap();
        assert_eq!(c.name, "redact");
        assert_eq!(c.args, vec!["--dry-run", "path/"]);
        assert!(c.recognized);
    }

    #[test]
    fn slash_is_optional_and_empty_is_none() {
        assert_eq!(parse_command("status").unwrap().name, "status");
        assert!(parse_command("   ").is_none());
    }

    #[test]
    fn unrecognized_command_is_flagged() {
        let c = parse_command("/bogus").unwrap();
        assert!(!c.recognized);
    }

    #[test]
    fn autocomplete_prefix_match() {
        assert_eq!(autocomplete("s"), vec!["scan", "setup", "status"]);
        assert_eq!(autocomplete("/ver"), vec!["verify"]);
        assert!(autocomplete("zzz").is_empty());
    }
}
