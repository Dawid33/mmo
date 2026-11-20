//! The chat box on screen. Enter or `/` opens it (`/` with the slash typed);
//! in first person T does too (in the commander T is the Chop tool). Esc
//! closes it and drops the draft, Enter submits and closes, Up / Down walk
//! the lines sent this session.
//!
//! While it is open the typing gate (`text_entry`) keeps every key from the
//! game, so everything here reads `KeyboardInput` messages, never
//! `ButtonInput`. The opening key is not typed into the field: Slint was
//! given that key press (`slint_overlay::slint_input`, `PreUpdate`) before
//! the field had the keyboard.
//!
//! The open box is in the interactive game UI; the newest lines shown while
//! it is closed are in the pass-through HUD, so they never take a click.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use slint_ui::slint::{ModelRc, SharedString, VecModel};
use slint_ui::{ChatRow, GameUi, HudUi};

use super::command::{Submitted, submit};
use super::{ChatKind, ChatLine, ChatLog};
use crate::renderer::commander::first_person::FirstPerson;
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::slint_overlay::SlintLayer;
use crate::renderer::text_entry::TextEntry;
use crate::session::SessionLocal;

/// One submitted line, read (`command::submit`). Written once per Enter.
#[derive(Message, Debug, Clone)]
pub struct ChatSubmitted(pub Submitted);

/// Lines the field reported on Enter, until `chat_accept` takes them.
pub struct ChatAccepted(Rc<RefCell<Vec<String>>>);

/// Connects the game UI's chat callback (from `slint_overlay::connect_game_ui`).
pub fn connect(ui: &GameUi, world: &mut World) {
    let accepted = Rc::new(RefCell::new(Vec::new()));
    let queue = accepted.clone();
    ui.on_chat_accepted(move |text| queue.borrow_mut().push(text.to_string()));
    world.insert_non_send(ChatAccepted(accepted));
}

/// The draft the chat opens with for a key event, if it opens it: Enter and
/// T (first person only) open it empty, `/` with the slash. A repeated press
/// (a held key) opens nothing, so the Enter that submitted a line does not
/// reopen the box.
pub fn opening_key(
    key: &Key,
    state: ButtonState,
    repeat: bool,
    first_person: bool,
) -> Option<&'static str> {
    if state != ButtonState::Pressed || repeat {
        return None;
    }
    match key {
        Key::Enter => Some(""),
        Key::Character(c) if c.as_str() == "/" => Some("/"),
        Key::Character(c) if first_person && c.as_str().eq_ignore_ascii_case("t") => Some(""),
        _ => None,
    }
}

/// Where Up (`up`) or Down moves in `len` sent lines from `at` (`None` = the
/// live draft, below the newest).
pub fn history_step(len: usize, at: Option<usize>, up: bool) -> Option<usize> {
    match (at, up) {
        (None, true) => len.checked_sub(1),
        (Some(i), true) => Some(i.saturating_sub(1)),
        (Some(i), false) if i + 1 < len => Some(i + 1),
        (_, false) => None,
    }
}

/// Where in the sent lines Up / Down stands.
#[derive(Default)]
pub struct HistoryAt(Option<usize>);

fn close(log: &mut ChatLog, ui: Option<&GameUi>) {
    log.open = false;
    if let Some(ui) = ui {
        ui.set_chat_draft(SharedString::new());
        ui.invoke_chat_drop_focus();
    }
}

/// A key opens the chat, unless a menu, the map, the inventory, an overlay or
/// the loading screen has the game.
pub fn open_chat(
    mut events: MessageReader<KeyboardInput>,
    policy: Option<Res<CursorPolicy>>,
    first_person: Option<Res<FirstPerson>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut log: ResMut<ChatLog>,
    mut history: SessionLocal<HistoryAt>,
) {
    let Some(slint) = slint else {
        events.clear();
        return;
    };
    if log.open || policy.is_some_and(|p| p.blocks_game()) {
        events.clear();
        return;
    }
    let first_person = first_person.is_some_and(|fp| fp.on);
    let Some(draft) = events
        .read()
        .find_map(|e| opening_key(&e.logical_key, e.state, e.repeat, first_person))
    else {
        return;
    };
    events.clear();
    log.open = true;
    history.get().0 = None;
    let ui = slint.overlay.ui();
    ui.set_chat_draft(draft.into());
    ui.invoke_chat_take_focus();
}

/// While the chat is open: Esc closes it, Up / Down put a sent line in the
/// field.
pub fn chat_keys(
    mut events: MessageReader<KeyboardInput>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut log: ResMut<ChatLog>,
    mut history: SessionLocal<HistoryAt>,
) {
    if !log.open {
        events.clear();
        return;
    }
    let ui = slint.as_ref().map(|s| s.overlay.ui());
    let history = history.get();
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        let up = match event.logical_key {
            Key::Escape => {
                close(&mut log, ui);
                history.0 = None;
                return;
            }
            Key::ArrowUp => true,
            Key::ArrowDown => false,
            _ => continue,
        };
        let to = history_step(log.sent.len(), history.0, up);
        if to == history.0 {
            continue;
        }
        history.0 = to;
        if let Some(ui) = ui {
            let line = to.and_then(|i| log.sent.get(i)).map_or("", String::as_str);
            ui.set_chat_draft(line.into());
            ui.invoke_chat_take_focus();
        }
    }
}

/// Enter in the field: the line is read, remembered, logged if it is plain
/// chat, handed on as [`ChatSubmitted`], and the box closes.
pub fn chat_accept(
    accepted: Option<NonSend<ChatAccepted>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    time: Res<Time>,
    mut log: ResMut<ChatLog>,
    mut submitted: MessageWriter<ChatSubmitted>,
) {
    let Some(accepted) = accepted else { return };
    let lines: Vec<String> = accepted.0.borrow_mut().drain(..).collect();
    if !log.open {
        return;
    }
    // One Enter, one line: the box closes on it.
    let Some(line) = lines.into_iter().next() else {
        return;
    };
    let read = submit(&line);
    if read != Submitted::Empty {
        log.remember_sent(line.trim());
    }
    if let Submitted::Say(text) = &read {
        log.say(text, time.elapsed_secs_f64());
    }
    submitted.write(ChatSubmitted(read));
    close(&mut log, slint.as_ref().map(|s| s.overlay.ui()));
}

fn row(line: &ChatLine) -> ChatRow {
    let kind = match line.kind {
        ChatKind::Said => 0,
        ChatKind::Info => 1,
        ChatKind::Error => 2,
    };
    ChatRow {
        text: line.text.as_str().into(),
        kind,
    }
}

fn rows<'a>(lines: impl Iterator<Item = &'a ChatLine>) -> ModelRc<ChatRow> {
    ModelRc::new(VecModel::from(lines.map(row).collect::<Vec<_>>()))
}

/// What of the chat is on screen, to push only changes.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub struct ChatShown {
    /// `ChatLog::revision` + 1 of the lines in the box (0 = none pushed).
    lines: u64,
    /// The same for the HUD's recent lines, and how many of them.
    recent: (u64, usize),
}

/// The log → the game UI's box and the HUD's recent lines; the typing gate
/// follows the box.
pub fn sync_chat(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    hud: Option<NonSend<SlintLayer<HudUi>>>,
    time: Res<Time>,
    log: Res<ChatLog>,
    mut entry: ResMut<TextEntry>,
    mut shown: SessionLocal<ChatShown>,
) {
    let shown = shown.get();
    if entry.on != log.open {
        entry.on = log.open;
    }
    if let Some(slint) = slint {
        let ui = slint.overlay.ui();
        if ui.get_chat_open() != log.open {
            ui.set_chat_open(log.open);
        }
        if shown.lines != log.revision + 1 {
            ui.set_chat_lines(rows(log.lines.iter()));
            shown.lines = log.revision + 1;
        }
    }
    if let Some(hud) = hud {
        // Nothing under the open box: it shows every line itself.
        let count = if log.open {
            0
        } else {
            log.recent(time.elapsed_secs_f64()).count()
        };
        let now = (log.revision + 1, count);
        if shown.recent != now {
            let ui = hud.overlay.ui();
            if count == 0 {
                ui.set_chat_recent(rows(std::iter::empty()));
            } else {
                ui.set_chat_recent(rows(log.recent(time.elapsed_secs_f64())));
            }
            shown.recent = now;
        }
    }
}
