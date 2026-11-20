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
    SetEditor(bool),
    Give {
        item: game::ItemId,
        count: u32,
    },
    /// Every designation done at once (the debug pane's button).
    #[cfg(feature = "dev_ui")]
    Complete,
    /// Drop a 1 m body at the cursor (the debug pane's button).
    #[cfg(feature = "dev_ui")]
    DropBody,
    /// Flip one of the debug pane's switches.
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
    "/creative - enter editor mode",
    "/normal - return to normal play",
    "/gamemode creative|normal - switch editor mode",
    "/give <item> [count] - give an editor item (default 64)",
    #[cfg(feature = "dev_ui")]
    "/complete - finish every designation at once",
    #[cfg(feature = "dev_ui")]
    "/dropbody - drop a test body at the cursor",
    #[cfg(feature = "dev_ui")]
    "/toggle boundaries|grid|culling|lod|near|colliders|xray - flip a debug view",
];

/// The names `/toggle` takes, in the debug pane's order.
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
        Some(extra) => Err(format!(
            "/{verb} takes nothing after it (got \"{extra}\"). Try /help"
        )),
    };
    match verb {
        "help" => alone(Command::Help),
        "adventure" => alone(Command::Adventure),
        "leave" => alone(Command::Leave),
        "where" => alone(Command::Where),
        "clear" => alone(Command::Clear),
        "creative" => alone(Command::SetEditor(true)),
        "normal" => alone(Command::SetEditor(false)),
        "gamemode" => match args {
            [mode] if mode.eq_ignore_ascii_case("creative") => Ok(Command::SetEditor(true)),
            [mode] if mode.eq_ignore_ascii_case("normal") => Ok(Command::SetEditor(false)),
            [mode] => Err(format!(
                "/gamemode does not know \"{mode}\" (use creative or normal)"
            )),
            _ => Err("/gamemode takes creative or normal".to_string()),
        },
        "give" => parse_give(args),
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

fn parse_give(args: &[&str]) -> Result<Command, String> {
    let item = args
        .first()
        .ok_or_else(|| "/give takes an item and an optional count".to_string())?;
    let item =
        game::item_named(item).ok_or_else(|| format!("/give does not know item \"{}\"", item))?;
    let count = match args {
        [_] => game::MAX_STACK,
        [_, count] => count
            .parse::<u32>()
            .map_err(|_| format!("/give count must be 1..={}", game::MAX_STACK))?,
        _ => return Err("/give takes an item and at most one count".to_string()),
    };
    if !(1..=game::MAX_STACK).contains(&count) {
        return Err(format!("/give count must be 1..={}", game::MAX_STACK));
    }
    Ok(Command::Give { item, count })
}
