//! In-game chat, client-side only for now: what the player types is kept in
//! memory for the session ([`ChatLog`]) and sent to nobody. A line starting
//! with `/` is a command (`command.rs`), run by `run.rs` as the same action a
//! button would take. `ui.rs` is the Slint box.
//!
//! Ordinary lines enter the log through [`ChatLog::say`] alone: that is where
//! a transport (Zulip, later) takes them.
//!
//! Design: `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md`.

use std::collections::VecDeque;

use bevy::prelude::*;

use crate::session::{SessionResetExt, world_systems_enabled};

pub mod command;
pub mod run;
pub mod ui;

/// The most characters one line holds; a longer one is cut.
pub const CHAT_LINE_MAX: usize = 256;
/// Lines the log keeps.
pub const CHAT_LOG_LINES: usize = 100;
/// Sent lines Up / Down walk through.
pub const CHAT_SENT_HISTORY: usize = 50;
/// Lines shown while the box is closed, and for how long each.
pub const CHAT_RECENT_LINES: usize = 8;
pub const CHAT_RECENT_SECS: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    /// What the player said.
    Said,
    /// A command's answer.
    Info,
    /// A command that could not be read or run.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatLine {
    pub kind: ChatKind,
    pub text: String,
    /// `Time::elapsed_secs_f64` when it was added.
    pub at: f64,
}

/// The session's chat: its lines, what was sent, and whether the box is open.
#[derive(Resource, Default, Debug)]
pub struct ChatLog {
    pub lines: VecDeque<ChatLine>,
    /// Submitted lines, oldest first.
    pub sent: VecDeque<String>,
    pub open: bool,
    /// Bumped by every change to `lines`.
    pub revision: u64,
}

impl ChatLog {
    fn push(&mut self, kind: ChatKind, text: String, now: f64) {
        self.lines.push_back(ChatLine {
            kind,
            text,
            at: now,
        });
        while self.lines.len() > CHAT_LOG_LINES {
            self.lines.pop_front();
        }
        self.revision += 1;
    }

    /// The player's own ordinary line. The one door chat leaves the client
    /// through, once it leaves it.
    pub fn say(&mut self, text: &str, now: f64) {
        self.push(ChatKind::Said, text.to_string(), now);
    }

    pub fn info(&mut self, text: impl Into<String>, now: f64) {
        self.push(ChatKind::Info, text.into(), now);
    }

    pub fn error(&mut self, text: impl Into<String>, now: f64) {
        self.push(ChatKind::Error, text.into(), now);
    }

    /// Remembers a submitted line for Up / Down; a repeat of the last one is
    /// kept once.
    pub fn remember_sent(&mut self, line: &str) {
        if self.sent.back().is_some_and(|last| last == line) {
            return;
        }
        self.sent.push_back(line.to_string());
        while self.sent.len() > CHAT_SENT_HISTORY {
            self.sent.pop_front();
        }
    }

    /// Empties the lines; what was sent stays.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.revision += 1;
    }

    /// The lines shown while the box is closed: the newest
    /// [`CHAT_RECENT_LINES`] younger than [`CHAT_RECENT_SECS`], oldest first.
    pub fn recent(&self, now: f64) -> impl Iterator<Item = &ChatLine> {
        let fresh = self
            .lines
            .iter()
            .rev()
            .take(CHAT_RECENT_LINES)
            .take_while(|l| now - l.at < CHAT_RECENT_SECS)
            .count();
        self.lines.iter().skip(self.lines.len() - fresh)
    }
}

pub struct ChatPlugin;

impl Plugin for ChatPlugin {
    fn build(&self, app: &mut App) {
        app.add_session_reset::<ChatLog>()
            .add_message::<ui::ChatSubmitted>()
            .add_session_message_clear::<ui::ChatSubmitted>()
            .add_systems(
                Update,
                (
                    ui::open_chat,
                    ui::chat_keys,
                    ui::chat_accept,
                    run::run_commands,
                    ui::sync_chat,
                )
                    .chain()
                    .run_if(world_systems_enabled()),
            );
    }
}
