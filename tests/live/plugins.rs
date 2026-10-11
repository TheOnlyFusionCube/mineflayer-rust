//! Live ecosystem-plugin test — connects to the local offline-mode
//! fixture (`scripts/live-server.sh`, port 25566) as `u12bot` and
//! exercises the U12 surfaces end to end on a real world:
//!
//! 1. `collect_block().collect(kind).count(2)` on the nearest tree:
//!    `CollectBlockFound` → `Collected` → `CollectDone` fire in order,
//!    the dug cells turn to air, and one trunk hole is patched back
//!    with the spare log where practical.
//! 2. `tool()`: the other log is crafted (2x2, no table) into a wooden
//!    shovel and equipped, then `best_tool_for`/`equip_for_block` must
//!    pick it for dirt; `equip_for_entity` must land on the same slot —
//!    the shovel is the best melee item in an otherwise-bare hotbar.
//! 3. `pvp()`: attack whatever attackable mob the fixture has loaded —
//!    `target()` records it, `stop()` ends the loop with exactly one
//!    `StoppedAttacking`. No mobs nearby → the phase skips.
//!
//! Only compiled with `--features live`; run with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live_plugins -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use azalea::{
    BlockPos, Vec3,
    entity::EntityKindComponent,
    registry::builtin::{BlockKind, EntityKind, ItemKind},
};
use mineflayer::{Bot, BotEvent, CollectTarget, EquipDest, FindBlocksOpts};
use tokio::sync::{
    broadcast::{Receiver, error::RecvError},
    mpsc,
};

/// Receive events until one satisfies `pred` (`Lagged` just re-reads).
/// Panics on timeout or a closed stream.
async fn wait_for(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> BotEvent {
    tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("event stream closed while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

/// Forward only matching events into an unbounded channel.
///
/// The fixture world streams ~1k entity-packet events a second; the
/// `BotEvent` ring (8192) overflows inside a single test phase, so a
/// receiver left unread across a phase loses everything mid-phase.
/// A task that `recv()`s continuously drains in real time — subscribe
/// per phase and read the tap instead of the raw receiver.
fn tap(
    bot: &Bot,
    pred: impl Fn(&BotEvent) -> bool + Send + 'static,
) -> mpsc::UnboundedReceiver<BotEvent> {
    let mut rx = bot.events();
    let (tx, out) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) if pred(&e) => {
                    if tx.send(e).is_err() {
                        return;
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
    out
}

/// Next event off a [`tap`], or panic if it starves for 10s.
async fn tapped(rx: &mut mpsc::UnboundedReceiver<BotEvent>, what: &str) -> BotEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("event tap closed")
}

/// Poll `cond` until it holds or `timeout` expires.
async fn until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

/// Collectable kinds in preference order: logs first (they feed the
/// tool phase), then common diggable ground blocks as the collect-only
/// fallback.
const LOGS: &[BlockKind] = &[
    BlockKind::OakLog,
    BlockKind::BirchLog,
    BlockKind::SpruceLog,
    BlockKind::JungleLog,
    BlockKind::AcaciaLog,
    BlockKind::DarkOakLog,
    BlockKind::MangroveLog,
    BlockKind::CherryLog,
];

const FALLBACK_DIGGABLE: &[BlockKind] = &[
    BlockKind::Dirt,
    BlockKind::GrassBlock,
    BlockKind::Sand,
    BlockKind::Gravel,
    BlockKind::Clay,
];

/// Kinds `pvp.attack` is allowed to drive at — whitelisted so
/// `nearest_entity` can't hand back an item drop, the bot itself, or
/// a projectile.
const ATTACKABLE: &[EntityKind] = &[
    EntityKind::Zombie,
    EntityKind::Husk,
    EntityKind::Skeleton,
    EntityKind::Stray,
    EntityKind::Spider,
    EntityKind::CaveSpider,
    EntityKind::Creeper,
    EntityKind::Enderman,
    EntityKind::Witch,
    EntityKind::Slime,
    EntityKind::ZombieVillager,
    EntityKind::Pillager,
    EntityKind::Pig,
    EntityKind::Cow,
    EntityKind::Sheep,
    EntityKind::Chicken,
    EntityKind::Horse,
    EntityKind::Donkey,
    EntityKind::Rabbit,
    EntityKind::Wolf,
    EntityKind::Villager,
];

fn log_item_kind(block: BlockKind) -> Option<ItemKind> {
    Some(match block {
        BlockKind::OakLog => ItemKind::OakLog,
        BlockKind::BirchLog => ItemKind::BirchLog,
        BlockKind::SpruceLog => ItemKind::SpruceLog,
        BlockKind::JungleLog => ItemKind::JungleLog,
        BlockKind::AcaciaLog => ItemKind::AcaciaLog,
        BlockKind::DarkOakLog => ItemKind::DarkOakLog,
        BlockKind::MangroveLog => ItemKind::MangroveLog,
        BlockKind::CherryLog => ItemKind::CherryLog,
        _ => return None,
    })
}

fn planks_item_kind(block: BlockKind) -> Option<ItemKind> {
    Some(match block {
        BlockKind::OakLog => ItemKind::OakPlanks,
        BlockKind::BirchLog => ItemKind::BirchPlanks,
        BlockKind::SpruceLog => ItemKind::SprucePlanks,
        BlockKind::JungleLog => ItemKind::JunglePlanks,
        BlockKind::AcaciaLog => ItemKind::AcaciaPlanks,
        BlockKind::DarkOakLog => ItemKind::DarkOakPlanks,
        BlockKind::MangroveLog => ItemKind::MangrovePlanks,
        BlockKind::CherryLog => ItemKind::CherryPlanks,
        _ => return None,
    })
}

/// What a dug block actually drops as an item — grass blocks drop
/// dirt, and clay breaks into unplaceable balls (`None` → no restore).
fn drop_item_kind(block: BlockKind) -> Option<ItemKind> {
    match block {
        BlockKind::GrassBlock => Some(ItemKind::Dirt),
        BlockKind::Dirt => Some(ItemKind::Dirt),
        BlockKind::Sand => Some(ItemKind::Sand),
        BlockKind::Gravel => Some(ItemKind::Gravel),
        _ => log_item_kind(block),
    }
}

/// Put a block back into `hole` using the placeable drop in the
/// inventory — the block above first (a standing trunk), then the
/// four side walls like the interact test.
async fn restore_block(bot: &Bot, hole: BlockPos) -> bool {
    let refs = [
        (
            BlockPos {
                y: hole.y + 1,
                ..hole
            },
            Vec3::new(0., -1., 0.),
        ),
        (
            BlockPos {
                x: hole.x + 1,
                ..hole
            },
            Vec3::new(-1., 0., 0.),
        ),
        (
            BlockPos {
                x: hole.x - 1,
                ..hole
            },
            Vec3::new(1., 0., 0.),
        ),
        (
            BlockPos {
                z: hole.z + 1,
                ..hole
            },
            Vec3::new(0., 0., -1.),
        ),
        (
            BlockPos {
                z: hole.z - 1,
                ..hole
            },
            Vec3::new(0., 0., 1.),
        ),
    ];
    for (ref_pos, face) in refs {
        let Some(reference) = bot.block_at(ref_pos) else {
            continue;
        };
        if reference.state.is_air() {
            continue;
        }
        if bot.place_block(&reference, face).await.is_ok() {
            return true;
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn collect_tool_pvp() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u12bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();

    wait_for(
        &mut rx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "spawn",
    )
    .await;
    bot.wait_for_chunks_to_load()
        .await
        .expect("chunks never loaded");

    // ----------------------------------------------------------------
    // collect: dig matching blocks through the find→goto→dig loop.
    // Logs are preferred (they feed the tool phase); ask for 2 so the
    // count loop iterates and one log can restore the hole.
    // ----------------------------------------------------------------
    let mut collect_tap = tap(&bot, |e| {
        matches!(
            e,
            BotEvent::CollectBlockFound(_) | BotEvent::Collected(_) | BotEvent::CollectDone { .. }
        )
    });
    let mut collected = None;
    'kinds: for kind in LOGS.iter().chain(FALLBACK_DIGGABLE.iter()) {
        let want = if LOGS.contains(kind) { 2 } else { 1 };
        match bot
            .collect_block()
            .collect(CollectTarget::new(*kind).count(want).max_distance(48.))
            .await
        {
            Ok(c) => {
                collected = Some((*kind, c));
                break 'kinds;
            }
            Err(e) => eprintln!("collect {kind:?} failed: {e:?} — next kind"),
        }
    }
    let (collected_kind, collected) =
        collected.expect("collect found no diggable block within 48m — fixture has no world?");
    let is_log = LOGS.contains(&collected_kind);
    eprintln!(
        "collect: {collected_kind:?} x{} at {:?}",
        collected.blocks.len(),
        collected.blocks.iter().map(|b| b.pos).collect::<Vec<_>>()
    );

    // `CollectBlockFound` fires per *candidate attempt* — a found block
    // that turns out unreachable is skipped and the next candidate gets
    // its own Found — so Found events are a superset of the dug list.
    // Drain the tap through `CollectDone`, then check the ordering
    // contract: every dug block was found first, the Collected stream
    // matches `collected.blocks` in order, and Done reports the count.
    let (mut found, mut dug_events) = (vec![], vec![]);
    let done = loop {
        match tapped(&mut collect_tap, "CollectDone").await {
            BotEvent::CollectBlockFound(b) => found.push(b.pos),
            BotEvent::Collected(b) => dug_events.push(b.pos),
            BotEvent::CollectDone { collected: n } => break n,
            other => panic!("unexpected event on the collect tap: {other:?}"),
        }
    };
    assert_eq!(done, collected.blocks.len());
    let dug_positions: Vec<_> = collected.blocks.iter().map(|b| b.pos).collect();
    assert_eq!(dug_events, dug_positions);
    for pos in &dug_positions {
        assert!(found.contains(pos), "dug block {pos} never announced found");
        assert!(
            bot.block_at(*pos).is_some_and(|b| b.state.is_air()),
            "dug cell at {pos} isn't air"
        );
    }

    // Restore where practical: the last hole gets its drop back. A log
    // slated for the tool chain isn't restored — it's consumed.
    let restorable = drop_item_kind(collected_kind).and_then(|kind| {
        // keep one log back for crafting; a second one can patch the hole
        let spare = !is_log || collected.blocks.len() > 1;
        spare.then(|| bot.find_inventory_item(kind)).flatten()
    });
    if let Some(item) = restorable {
        let hole = collected.blocks.last().unwrap().pos;
        if bot.equip(&item, EquipDest::Hand).await.is_ok() {
            let restored = restore_block(&bot, hole).await;
            eprintln!("collect restore ({hole}): {restored}");
        }
    } else {
        eprintln!("collect restore skipped (drop not placeable / consumed)");
    }

    // ----------------------------------------------------------------
    // tool: a real best-tool pick needs a real tool. Craft a wooden
    // shovel from the collected log entirely on the 2x2 grid
    // (log → planks → sticks → shovel), then score it for dirt.
    // ----------------------------------------------------------------
    let planks_kind = if is_log {
        planks_item_kind(collected_kind)
    } else {
        eprintln!("SKIP tool phase: no log collected (no real tool to score)");
        None
    };
    let tool_chain: Result<(), String> = async {
        let Some(planks_kind) = planks_kind else {
            return Err("no log".into());
        };
        for (kind, times) in [
            (planks_kind, 1),
            (ItemKind::Stick, 1),
            (ItemKind::WoodenShovel, 1),
        ] {
            if !until(Duration::from_secs(10), || {
                !bot.recipes_for(kind, None, None, None).is_empty()
            })
            .await
            {
                return Err(format!("{kind:?} recipe never unlocked"));
            }
            let recipe = bot.recipes_for(kind, None, None, None)[0].clone();
            bot.craft(&recipe, Some(times), None)
                .await
                .map_err(|e| format!("craft {kind:?}: {e:?}"))?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = tool_chain {
        eprintln!("SKIP tool phase: {e}");
    } else {
        bot.equip_kind(ItemKind::WoodenShovel, EquipDest::Hand)
            .await
            .expect("equip shovel failed");
        let Some(dirt_pos) = [
            BlockKind::Dirt,
            BlockKind::GrassBlock,
            BlockKind::Sand,
            BlockKind::Gravel,
        ]
        .iter()
        .find_map(|k| {
            bot.find_blocks(FindBlocksOpts::new(*k).max_distance(16.).count(1))
                .into_iter()
                .next()
        }) else {
            panic!("no dirt-ish block in loaded chunks")
        };
        let dirt = bot.block_at(dirt_pos).expect("dirt unloaded");

        let best = bot
            .tool()
            .best_tool_for(&dirt)
            .expect("a shovel in the hotbar must beat the bare hand");
        assert_eq!(best.kind(), ItemKind::WoodenShovel);
        let shovel_hotbar = (best.slot - 36) as u8;

        bot.tool()
            .equip_for_block(&dirt)
            .await
            .expect("equip failed");
        assert_eq!(bot.quick_bar_slot(), shovel_hotbar);

        // max-damage pick: the shovel (+1.5) is the only weapon we
        // have, so equip_for_entity lands on the same slot
        let self_ref = bot.client().entity_ref_for(bot.client().entity);
        bot.tool()
            .equip_for_entity(&self_ref)
            .await
            .expect("equip_for_entity failed");
        assert_eq!(bot.quick_bar_slot(), shovel_hotbar);
    }

    // ----------------------------------------------------------------
    // pvp: attack whatever attackable mob the fixture has loaded.
    // ----------------------------------------------------------------
    let target = bot.nearest_entity(|e| {
        e.get_component::<EntityKindComponent>()
            .is_some_and(|k| ATTACKABLE.contains(&k.0))
    });
    match target {
        Some(target) => {
            let mut stop_tap = tap(&bot, |e| matches!(e, BotEvent::StoppedAttacking));
            bot.pvp().attack(&target).expect("pvp.attack failed");
            assert_eq!(
                bot.pvp().target().map(|t| t.id()),
                Some(target.id()),
                "target() didn't record the attacked entity"
            );
            // let the follow+swing loop run a moment, then stop
            tokio::time::sleep(Duration::from_secs(3)).await;
            bot.pvp().stop();
            assert!(bot.pvp().target().is_none(), "stop() left a target");
            match tapped(&mut stop_tap, "StoppedAttacking").await {
                BotEvent::StoppedAttacking => {}
                _ => unreachable!(),
            }
            // exactly once — whoever ends the loop owns the report
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                stop_tap.try_recv().is_err(),
                "StoppedAttacking fired more than once"
            );
        }
        None => eprintln!("SKIP pvp phase: no attackable entity loaded"),
    }

    // let queued packets drain before disconnecting — the bridge can hit a
    // teardown race if packets are still mid-translation
    let mut end_tap = tap(&bot, |e| matches!(e, BotEvent::End(_)));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    bot.quit(None);
    match tapped(&mut end_tap, "end").await {
        BotEvent::End(_) => {}
        _ => unreachable!(),
    }
}
