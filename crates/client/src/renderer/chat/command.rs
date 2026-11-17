//! What a submitted chat line is: nothing, something said, or a command.
//! Commands are few and plain, so the parser is by hand (no `clap`: its size
//! in the wasm build buys nothing here). A line that cannot be read is never
//! sent anywhere; its error names what was wrong.

#[cfg(feature = "dev_ui")]
use crate::renderer::dev_ui::DevToggle;

use super::CHAT_LINE_MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    /// Start an adventure at the site picked on the Adventure tab.
    Adventure,
    /// End the adventure.
    Leave,
    Where,
    Clear,
    /// Every designation done at once (the Debug tab's button).
    #[cfg(feature = "dev_ui")]
    Complete,
    /// Drop a 1 m body at the cursor (the Debug tab's button).
    #[cfg(feature = "dev_ui")]
    DropBody,
    /// Flip one of the Debug tab's switches.
    #[cfg(feature = "dev_ui")]
    Toggle(DevToggle),
}

/// One line per command, shown by `/help`.
pub const HELP: &[&str] = &[
    "/help - this list",
    "/adventure - start an adventure at the site picked on the Adventure tab",
    "/leave - end the adventure",
    "/where - the region and block you are in",
    "/clear - empty the chat",
    #[cfg(feature = "dev_ui")]
    "/complete - finish every designation at once",
    #[cfg(feature = "dev_ui")]
    "/dropbody - drop a test body at the cursor",
    #[cfg(feature = "dev_ui")]
    "/toggle boundaries|grid|culling|lod|near|colliders|xray - flip a debug view",
];

/// The names `/toggle` takes, in the Debug tab's order.
#[cfg(feature = "dev_ui")]
pub const TOGGLE_NAMES: [(&str, DevToggle); 7] = [
    ("boundaries", DevToggle::Boundaries),
    ("grid", DevToggle::ChunkGrid),
    ("culling", DevToggle::CaveCulling),
    ("lod", DevToggle::HideLod),
    ("near", DevToggle::HideNear),
    ("colliders", DevToggle::Colliders),
    ("xray", DevToggle::Xray),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// Nothing but blanks: the box just closes.
    Empty,
    /// An ordinary line, trimmed and cut to [`CHAT_LINE_MAX`] characters.
    Say(String),
    Run(Command),
    /// A command that could not be read, with what to tell the player.
    Bad(String),
}

/// Reads one submitted line.
pub fn submit(line: &str) -> Submitted {
    let line = line.trim();
    if line.is_empty() {
        return Submitted::Empty;
    }
    let Some(rest) = line.strip_prefix('/') else {
        return Submitted::Say(line.chars().take(CHAT_LINE_MAX).collect());
    };
    let mut words = rest.split_whitespace();
    let verb = words.next().unwrap_or("").to_ascii_lowercase();
    let args: Vec<&str> = words.collect();
    match parse(&verb, &args) {
        Ok(command) => Submitted::Run(command),
        Err(why) => Submitted::Bad(why),
    }
}

fn parse(verb: &str, args: &[&str]) -> Result<Command, String> {
    let alone = |command: Command| match args.first() {
        None => Ok(command),
        Some(extra) => Err(format!("/{verb} takes nothing after it (got \"{extra}\"). Try /help")),
    };
    match verb {
        "help" => alone(Command::Help),
        "adventure" => alone(Command::Adventure),
        "leave" => alone(Command::Leave),
        "where" => alone(Command::Where),
        "clear" => alone(Command::Clear),
        #[cfg(feature = "dev_ui")]
        "complete" => alone(Command::Complete),
        #[cfg(feature = "dev_ui")]
        "dropbody" => alone(Command::DropBody),
        #[cfg(feature = "dev_ui")]
        "toggle" => match args {
            [name] => {
                let name = name.to_ascii_lowercase();
                TOGGLE_NAMES
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, toggle)| Command::Toggle(*toggle))
                    .ok_or_else(|| format!("/toggle does not know \"{name}\". Try /help"))
            }
            _ => Err("/toggle takes one name. Try /help".to_string()),
        },
        _ => Err(format!("Unknown command: /{verb}. Try /help")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_is_empty() {
        assert_eq!(submit(""), Submitted::Empty);
        assert_eq!(submit("   "), Submitted::Empty);
    }

    #[test]
    fn plain_text_is_said_trimmed() {
        assert_eq!(submit(" hello there "), Submitted::Say("hello there".into()));
    }

    #[test]
    fn a_long_line_is_cut_at_256_chars() {
        let Submitted::Say(said) = submit(&"é".repeat(300)) else { panic!("a line") };
        assert_eq!(said.chars().count(), CHAT_LINE_MAX, "characters, not bytes");
    }

    #[test]
    fn verbs_ignore_case() {
        assert_eq!(submit("/help"), Submitted::Run(Command::Help));
        assert_eq!(submit("/HELP"), Submitted::Run(Command::Help));
        assert_eq!(submit("  /where  "), Submitted::Run(Command::Where));
        assert_eq!(submit("/clear"), Submitted::Run(Command::Clear));
        assert_eq!(submit("/adventure"), Submitted::Run(Command::Adventure));
        assert_eq!(submit("/leave"), Submitted::Run(Command::Leave));
    }

    #[test]
    fn extra_arguments_are_refused() {
        assert_eq!(
            submit("/clear now"),
            Submitted::Bad("/clear takes nothing after it (got \"now\"). Try /help".into())
        );
    }

    #[test]
    fn unknown_verb() {
        assert_eq!(submit("/fly"), Submitted::Bad("Unknown command: /fly. Try /help".into()));
    }

    #[test]
    fn a_lone_slash_is_unknown() {
        assert_eq!(submit("/"), Submitted::Bad("Unknown command: /. Try /help".into()));
    }

    #[cfg(feature = "dev_ui")]
    #[test]
    fn toggle_takes_one_known_name() {
        assert_eq!(submit("/toggle grid"), Submitted::Run(Command::Toggle(DevToggle::ChunkGrid)));
        assert_eq!(submit("/toggle XRAY"), Submitted::Run(Command::Toggle(DevToggle::Xray)));
        assert_eq!(submit("/toggle"), Submitted::Bad("/toggle takes one name. Try /help".into()));
        assert_eq!(submit("/toggle fog"), Submitted::Bad("/toggle does not know \"fog\". Try /help".into()));
        assert_eq!(TOGGLE_NAMES.map(|(_, t)| t), DevToggle::ALL, "every switch has a name");
    }

    #[test]
    fn every_command_has_a_help_line() {
        for line in HELP {
            let verb = line.split(' ').next().unwrap();
            assert!(!matches!(submit(verb), Submitted::Bad(why) if why.starts_with("Unknown")), "{verb} is listed but unknown");
        }
    }
}
