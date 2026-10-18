//! Guard a spot with the pvp and pathfinder plugins: attack monsters that
//! come within 16 blocks, then walk back. Port of mineflayer's
//! `examples/guard.js`.
//!
//! ```sh
//! cargo run --example guard -- <host> <port> [<name>]
//! ```
//! Chat: `guard` (guard where the speaker stands), `stop`.

mod common;

use futures::StreamExt;
use mineflayer::{engine::entity::metadata::AbstractMonster, prelude::*};
use mineflayer_plugins::{prelude::*, pvp::PvpEvent};

#[tokio::main]
async fn main() {
    let bot = common::connect("guard", "Guard").await;
    let mut events = bot.event_stream();
    let mut pvp_events = bot.plugin_events::<PvpEvent>();
    let mut guard_pos: Option<BlockPos> = None;
    loop {
        tokio::select! {
            Ok(PvpEvent::StoppedAttacking) = pvp_events.recv() => {
                if let Some(pos) = guard_pos {
                    move_to(&bot, pos);
                }
            }
            event = events.next() => match event {
                Some(BotEvent::Tick) => {
                    let Some(pos) = guard_pos else { continue };
                    let near = |e: &EntityRef| {
                        e.get_component::<AbstractMonster>().is_some()
                            && e.position().distance_to(bot.position()) < 16.
                    };
                    if let Some(enemy) = bot.nearest_entity(near) {
                        let _ = bot.pvp().attack(&enemy);
                    } else if bot.pvp().target().is_none() && !bot.pathfinder().have_goal() {
                        move_to(&bot, pos);
                    }
                }
                Some(BotEvent::Chat(packet)) => {
                    let (Some(username), message) = packet.split_sender_and_content() else {
                        continue;
                    };
                    match message.as_str() {
                        "guard" => match bot.players().get(&username).and_then(|p| p.entity.clone()) {
                            Some(player) => {
                                let _ = bot.chat("I will guard that location.");
                                let pos = BlockPos::from(player.position());
                                guard_pos = Some(pos);
                                if bot.pvp().target().is_none() {
                                    move_to(&bot, pos);
                                }
                            }
                            None => {
                                let _ = bot.chat("I can't see you.");
                            }
                        },
                        "stop" => {
                            let _ = bot.chat("I will no longer guard this area.");
                            guard_pos = None;
                            bot.pvp().stop();
                            bot.pathfinder().stop();
                        }
                        _ => {}
                    }
                }
                Some(BotEvent::End(_)) | None => break,
                Some(_) => {}
            },
        }
    }
}

fn move_to(bot: &Bot, pos: BlockPos) {
    bot.pathfinder().set_movements(Movements::default());
    let _ = bot.pathfinder().set_goal(Goal::Block(pos), false);
}
