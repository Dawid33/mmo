//! Running a chat command: [`effects`] decides, purely, what a command does
//! given what the client knows ([`RunContext`]); [`run_commands`] gathers
//! that, and carries the effects out. A command that changes the world does
//! it through the action its button takes, so it is predicted and replayed
//! like a click; what it prints is a local line, printed once.

use bevy::prelude::*;
use game::RegionCoords;

use super::command::{Command, Submitted, HELP};
use super::ui::ChatSubmitted;
use super::ChatLog;
#[cfg(feature = "dev_ui")]
use crate::renderer::dev_ui::{self, DevAction, DevToggle};
use crate::renderer::bridge::{LocalBody, SimEntity, SimTarget};
use crate::renderer::history_view::HistoryView;
use crate::renderer::hud::HudStatus;
use crate::renderer::slint_overlay;

/// What the client knows when a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunContext {
    pub home: Option<RegionCoords>,
    /// The block the local body's centre is in, local to `home`.
    pub body_block: Option<[i32; 3]>,
    /// On an adventure now.
    pub adventuring: bool,
    /// A start or an end is out and unanswered.
    pub adventure_pending: bool,
    /// The Adventure tab has a site picked.
    pub site_picked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Info(String),
    Error(String),
    ClearLog,
    /// What the Adventure tab's start button does.
    StartAdventure,
    /// What the End-adventure button does.
    EndAdventure,
    #[cfg(feature = "dev_ui")]
    Complete,
    #[cfg(feature = "dev_ui")]
    DropBody,
    #[cfg(feature = "dev_ui")]
    Flip(DevToggle),
}

/// What `command` does, in order.
pub fn effects(command: Command, ctx: &RunContext) -> Vec<Effect> {
    let info = |text: &str| Effect::Info(text.to_string());
    let error = |text: &str| Effect::Error(text.to_string());
    match command {
        Command::Help => HELP.iter().map(|line| info(line)).collect(),
        Command::Clear => vec![Effect::ClearLog],
        Command::Where => match (ctx.home, ctx.body_block) {
            (Some(home), Some([x, y, z])) => {
                vec![Effect::Info(format!("Region ({}, {}), block {x} {y} {z}", home.x, home.z))]
            }
            (Some(home), None) => vec![Effect::Info(format!("Region ({}, {}), no body here", home.x, home.z))],
            (None, _) => vec![error("Not in a region yet")],
        },
        Command::Adventure if ctx.adventure_pending => vec![error("An adventure is already starting or ending")],
        Command::Adventure if ctx.adventuring => vec![error("Already on an adventure")],
        Command::Adventure if !ctx.site_picked => vec![error("Pick a site on the Adventure tab first")],
        Command::Adventure => vec![Effect::StartAdventure, info("Starting the adventure")],
        Command::Leave if ctx.adventure_pending => vec![error("An adventure is already starting or ending")],
        Command::Leave if !ctx.adventuring => vec![error("Not on an adventure")],
        Command::Leave => vec![Effect::EndAdventure, info("Ending the adventure")],
        #[cfg(feature = "dev_ui")]
        Command::Complete => vec![Effect::Complete, info("Every designation done")],
        #[cfg(feature = "dev_ui")]
        Command::DropBody => vec![Effect::DropBody, info("Dropped a body at the cursor")],
        #[cfg(feature = "dev_ui")]
        Command::Toggle(toggle) => vec![Effect::Flip(toggle)],
    }
}

/// The block (1 m) a region-local sim position is in.
pub fn block_of(pos: Vec3) -> [i32; 3] {
    let size = game::BLOCK_VOXELS as f32;
    [(pos.x / size).floor() as i32, (pos.y / size).floor() as i32, (pos.z / size).floor() as i32]
}

fn context(world: &mut World) -> RunContext {
    let home = world.get_resource::<HudStatus>().and_then(|s| s.home_region);
    let mut bodies = world.query_filtered::<(&SimEntity, &SimTarget), With<LocalBody>>();
    let body_block = bodies.iter(world).find(|(sim, _)| Some(sim.region) == home).map(|(_, target)| block_of(target.pos));
    let history = world.get_resource::<HistoryView>();
    RunContext {
        home,
        body_block,
        adventuring: history.is_some_and(|h| h.adventuring.is_some()),
        adventure_pending: history.is_some_and(|h| h.adventure_pending),
        site_picked: history.is_some_and(|h| h.adventure.site.is_some()),
    }
}

/// This frame's submitted lines: a command runs, a line that could not be
/// read prints why. (Ordinary lines were logged by `ui::chat_accept`.)
pub fn run_commands(world: &mut World) {
    let submitted: Vec<Submitted> = match world.get_resource_mut::<Messages<ChatSubmitted>>() {
        Some(mut messages) => messages.drain().map(|m| m.0).collect(),
        None => return,
    };
    if submitted.is_empty() {
        return;
    }
    let now = world.resource::<Time>().elapsed_secs_f64();
    for line in submitted {
        let command = match line {
            Submitted::Run(command) => command,
            Submitted::Bad(why) => {
                world.resource_mut::<ChatLog>().error(why, now);
                continue;
            }
            Submitted::Empty | Submitted::Say(_) => continue,
        };
        let ctx = context(world);
        for effect in effects(command, &ctx) {
            apply(effect, world, now);
        }
    }
}

fn apply(effect: Effect, world: &mut World, now: f64) {
    match effect {
        Effect::Info(text) => world.resource_mut::<ChatLog>().info(text, now),
        Effect::Error(text) => world.resource_mut::<ChatLog>().error(text, now),
        Effect::ClearLog => world.resource_mut::<ChatLog>().clear(),
        Effect::StartAdventure => press(world, slint_overlay::UiAction::AdventureStart, now),
        Effect::EndAdventure => press(world, slint_overlay::UiAction::EndAdventure, now),
        #[cfg(feature = "dev_ui")]
        Effect::Complete => dev_ui::apply_dev_action(DevAction::CompleteOrders, world),
        #[cfg(feature = "dev_ui")]
        Effect::DropBody => dev_ui::apply_dev_action(DevAction::DropBody, world),
        #[cfg(feature = "dev_ui")]
        Effect::Flip(toggle) => {
            let on = !dev_ui::toggle_checked(toggle, world);
            dev_ui::apply_dev_action(DevAction::Toggle(toggle, on), world);
            let state = if on { "on" } else { "off" };
            world.resource_mut::<ChatLog>().info(format!("{}: {state}", toggle.label()), now);
        }
    }
}

/// Presses a game UI button for the player; says so when there is no game UI
/// to press it on.
fn press(world: &mut World, action: slint_overlay::UiAction, now: f64) {
    if !slint_overlay::queue_ui_action(world, action) {
        world.resource_mut::<ChatLog>().error("The game UI is not up", now);
    }
}

