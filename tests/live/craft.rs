//! Live crafting test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u10bot` and exercises the
//! U10 surface: the recipe book, `recipes_all`/`recipes_for` filters,
//! craft error paths, and — when a log is in pathfinding range — a real
//! end-to-end `craft` (punch a log → planks → crafting table on the 2x2
//! grid). Only compiled with `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live-craft -- --ignored
//! ```

#![cfg(feature = "live")]

use std::{str::FromStr, time::Duration};

use mineflayer::{
    Block, Bot, BotEvent, FindBlocksOpts, Goal, Ingredient, ItemKind, Recipe, RecipeResult,
};
use tokio::sync::broadcast::{Receiver, error::RecvError};

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

async fn connect() -> Bot {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u10bot")
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
    bot
}

/// Poll a predicate until it holds or the deadline passes (returns the
/// last value).
async fn until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Total count of `kind` in the player inventory.
fn count_of(bot: &Bot, kind: ItemKind) -> i32 {
    bot.inventory()
        .inventory_items()
        .iter()
        .filter(|i| i.kind() == kind)
        .map(|i| i.count())
        .sum()
}

/// `*_log`/`*_stem` block name → the item kind it drops and the planks
/// kind it crafts into (`oak_log` → `OakLog`/`OakPlanks`,
/// `crimson_stem` → `CrimsonStem`/`CrimsonPlanks`).
fn log_item_and_planks(block: &Block) -> Option<(ItemKind, ItemKind)> {
    let name = block.name();
    let base = name
        .strip_suffix("_log")
        .or_else(|| name.strip_suffix("_stem"))?;
    let item = ItemKind::from_str(&format!("minecraft:{name}"))
        .or_else(|_| ItemKind::from_str(name))
        .ok()?;
    let planks = ItemKind::from_str(&format!("minecraft:{base}_planks"))
        .or_else(|_| ItemKind::from_str(&format!("{base}_planks")))
        .ok()?;
    Some((item, planks))
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn recipe_book_and_craft() {
    let bot = connect().await;

    // ---- recipe book ---------------------------------------------------
    // vanilla unlocks the crafting-table recipe on join; the packet may
    // land a few ticks after spawn
    assert!(
        until(Duration::from_secs(10), || {
            bot.known_recipe_count() >= 1
        })
        .await,
        "server never sent a recipe book entry"
    );

    // ---- recipesAll ----------------------------------------------------
    let recipes = bot.recipes_all(ItemKind::CraftingTable, None, None);
    let recipe = recipes
        .first()
        .expect("recipe book should unlock minecraft:crafting_table");
    assert_eq!(recipe.result.kind, ItemKind::CraftingTable);
    assert_eq!(recipe.result.count, 1);
    assert!(!recipe.requires_table, "2x2 recipe must not need a table");
    let shape = recipe.in_shape.as_ref().expect("crafting table is shaped");
    assert_eq!(shape.len(), 2);
    assert_eq!(shape[0].len(), 2);
    // every cell is the #minecraft:planks tag → resolved alternatives
    for row in shape {
        for cell in row {
            let cell = cell.as_ref().expect("2x2 is fully occupied");
            assert!(
                cell.any_of.iter().any(|w| w.kind == ItemKind::OakPlanks),
                "#planks alternatives should include oak_planks"
            );
        }
    }
    // delta mirrors upstream: -4 planks (tag alternatives), +1 table
    assert_eq!(recipe.delta.len(), 2);
    assert_eq!(recipe.delta[0].count, -4);
    assert_eq!(recipe.delta[1].count, 1);

    // ---- recipesFor inventory gate --------------------------------------
    assert!(
        bot.recipes_for(ItemKind::CraftingTable, None, None, None)
            .is_empty(),
        "empty inventory shouldn't satisfy any recipe"
    );

    // ---- craft error paths ----------------------------------------------
    let err = bot
        .craft(recipe, Some(1), None)
        .await
        .expect_err("crafting without planks must fail");
    assert!(
        err.to_string().contains("missing ingredient"),
        "unexpected error: {err}"
    );

    let table_only = Recipe::new(
        RecipeResult {
            kind: ItemKind::Chest,
            count: 1,
            metadata: None,
        },
        Some(vec![
            vec![
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
            ],
            vec![
                Some(Ingredient::item(ItemKind::OakPlanks)),
                None,
                Some(Ingredient::item(ItemKind::OakPlanks)),
            ],
            vec![
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
            ],
        ]),
        None,
    );
    assert!(table_only.requires_table);
    let err = bot
        .craft(&table_only, None, None)
        .await
        .expect_err("3x3 recipe without a table must fail");
    assert!(
        err.to_string().contains("craftingTable"),
        "unexpected error: {err}"
    );

    // ---- end-to-end: punch a log → planks → crafting table --------------
    let logs = bot.find_blocks(
        FindBlocksOpts::new(|b: &Block| b.name().ends_with("_log") || b.name().ends_with("_stem"))
            .max_distance(48.)
            .count(8),
    );
    let mut crafted_table = false;
    for pos in logs {
        let Some((log_item, planks_kind)) =
            bot.block_at(pos).as_ref().and_then(log_item_and_planks)
        else {
            continue;
        };
        // get within reach, re-resolve (it may have been dug already),
        // and dig it
        if tokio::time::timeout(
            Duration::from_secs(45),
            bot.pathfinder().goto(Goal::GetToBlock(pos)),
        )
        .await
        .is_err()
        {
            continue;
        }
        let Some(log) = bot.block_at(pos).filter(|b| b.diggable()) else {
            continue;
        };
        if bot.dig(&log).await.is_err() {
            continue;
        }
        // walk onto the drop and wait for it to land in the inventory
        let _ = tokio::time::timeout(
            Duration::from_secs(15),
            bot.pathfinder().goto(Goal::Near {
                pos: pos.to_vec3_floored(),
                radius: 1.,
            }),
        )
        .await;
        if !until(Duration::from_secs(10), || count_of(&bot, log_item) >= 1).await {
            continue;
        }

        // picking up the log unlocks the planks recipe; craft it
        if !until(Duration::from_secs(10), || {
            !bot.recipes_for(planks_kind, None, None, None).is_empty()
        })
        .await
        {
            continue;
        }
        let planks_recipe = bot.recipes_for(planks_kind, None, None, None)[0].clone();
        let before = count_of(&bot, planks_kind);
        bot.craft(&planks_recipe, Some(1), None)
            .await
            .expect("planks craft failed");
        assert!(
            until(Duration::from_secs(5), || {
                count_of(&bot, planks_kind) >= before + planks_recipe.result.count
            })
            .await,
            "planks never appeared in inventory"
        );

        // four planks is exactly one crafting table on the 2x2 grid
        let table_recipes = bot.recipes_for(ItemKind::CraftingTable, None, None, None);
        if table_recipes.is_empty() {
            eprintln!("not enough planks for the crafting table — stopping at planks");
            break;
        }
        bot.craft(&table_recipes[0], None, None)
            .await
            .expect("crafting-table craft failed");
        assert!(
            until(Duration::from_secs(5), || {
                count_of(&bot, ItemKind::CraftingTable) >= 1
            })
            .await,
            "crafting table never appeared in inventory"
        );
        crafted_table = true;
        break;
    }
    if !crafted_table {
        eprintln!("no diggable log within 48 blocks — end-to-end craft skipped");
    }

    bot.quit(None);
}
