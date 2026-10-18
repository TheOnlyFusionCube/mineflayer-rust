use mineflayer_engine::{
    entity::{inventory::Inventory, metadata::Health},
    inventory::ItemStack,
    local_player::Hunger,
    registry::identifier::Identifier,
    world::WorldName,
};

use super::*;
use crate::test_util::{bot_with_shutdown, live_entity_client, tick_client, tick_sender};

/// An inventory carrying `items` (`slot` = absolute player-window
/// index — hotbar is 36..45, main inventory 9..36).
fn food_inventory(items: &[(usize, ItemKind)]) -> Inventory {
    let mut inventory = Inventory::default();
    for (slot, kind) in items {
        *inventory.inventory_menu.slot_mut(*slot).unwrap() = ItemStack::new(*kind, 1);
    }
    inventory
}

/// A scaffold bot for direct `eat()` calls — live entity carrying the
/// inventory, no tick broadcast (the driver isn't spawned).
fn eating_bot(items: &[(usize, ItemKind)]) -> Bot {
    crate::test_util::bot(live_entity_client((
        WorldName(Identifier::from("minecraft:overworld")),
        food_inventory(items),
    )))
}

/// A tick-capable bot for driver tests, and a guard that plays the
/// bridge's exit when dropped.
fn monitored_bot(
    food: u32,
    health: f32,
    items: &[(usize, ItemKind)],
) -> (Bot, tokio_util::sync::DropGuard) {
    bot_with_shutdown(tick_client((
        WorldName(Identifier::from("minecraft:overworld")),
        food_inventory(items),
        Hunger {
            food,
            saturation: 0.,
        },
        Health(health),
    )))
}

/// Drain pending events; collect the autoEat event names in order.
fn auto_eat_events(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> Vec<&'static str> {
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(match event {
            BotEvent::EatStart { .. } => "eatStart",
            BotEvent::EatFail(_) => "eatFail",
            BotEvent::EatFinish { .. } => "eatFinish",
            _ => continue,
        });
    }
    seen
}

/// Send one tick once a receiver exists — `enable_auto`/spawned
/// drivers subscribe to the broadcast asynchronously, so a send
/// fired immediately can hit `SendError` before any `recv` is armed.
async fn send_tick(client: &mineflayer_engine::Client) {
    for _ in 0..200 {
        if tick_sender(client).send(()).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no tick receiver ever subscribed");
}

/// Wait (bounded) for the next EatStart, returning its food kind.
async fn next_eat_start(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> ItemKind {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("no EatStart within 5s")
            .expect("event stream closed");
        if let BotEvent::EatStart { food, .. } = event {
            return food.kind();
        }
    }
}

/// The scoring key follows minecraft-data's field names — cooked
/// beef and golden carrot disagree across priorities, so every
/// branch is observable.
#[test]
fn priority_scores_match_minecraft_data() {
    let beef = AutoEat::food_data(ItemKind::CookedBeef).unwrap();
    let carrot = AutoEat::food_data(ItemKind::GoldenCarrot).unwrap();
    assert_eq!((beef.nutrition, carrot.nutrition), (8, 6));
    assert!((beef.saturation - 12.8).abs() < 0.001);
    assert!((carrot.saturation - 14.4).abs() < 0.001);

    assert!(FoodPriority::FoodPoints.score(&beef) > FoodPriority::FoodPoints.score(&carrot));
    assert!(FoodPriority::Saturation.score(&carrot) > FoodPriority::Saturation.score(&beef));
    assert!(
        FoodPriority::EffectiveQuality.score(&beef) > FoodPriority::EffectiveQuality.score(&carrot)
    );
    assert!(
        FoodPriority::SaturationRatio.score(&carrot) > FoodPriority::SaturationRatio.score(&beef)
    );
    // literal mcdata `saturationRatio` values (saturation/foodPoints):
    // cooked beef 12.8/8 = 1.6, golden carrot 14.4/6 = 2.4
    assert!(
        (FoodPriority::SaturationRatio.score(&beef) - 1.6).abs() < 0.001,
        "beef ratio must be saturation/nutrition, not halved"
    );
    assert!((FoodPriority::SaturationRatio.score(&carrot) - 2.4).abs() < 0.001);

    // non-foods have no entry, like `name in foodsByName` upstream
    assert!(AutoEat::food_data(ItemKind::Stone).is_none());
}

#[test]
fn best_food_picks_highest_score_and_skips_banned() {
    let bot = eating_bot(&[
        (9, ItemKind::Apple),
        (10, ItemKind::CookedBeef),
        (11, ItemKind::RottenFlesh),
    ]);
    let auto_eat = bot.auto_eat();
    assert_eq!(
        auto_eat
            .best_food(FoodPriority::FoodPoints)
            .map(|i| i.kind()),
        Some(ItemKind::CookedBeef)
    );
    // golden carrot out-saturates beef; rotten flesh stays banned
    let bot = eating_bot(&[(9, ItemKind::CookedBeef), (10, ItemKind::GoldenCarrot)]);
    let auto_eat = bot.auto_eat();
    assert_eq!(
        auto_eat
            .best_food(FoodPriority::Saturation)
            .map(|i| i.kind()),
        Some(ItemKind::GoldenCarrot)
    );
    assert_eq!(
        auto_eat
            .best_food(FoodPriority::FoodPoints)
            .map(|i| i.kind()),
        Some(ItemKind::CookedBeef)
    );
    // an all-banned/non-food inventory has no choice
    let bot = eating_bot(&[(9, ItemKind::RottenFlesh), (10, ItemKind::Stone)]);
    assert!(bot.auto_eat().best_food(FoodPriority::FoodPoints).is_none());
}

#[tokio::test]
async fn eat_without_food_errors_before_any_event() {
    let bot = eating_bot(&[]);
    let mut rx = bot.events();
    let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
    assert!(err.to_string().contains("couldn't find a choice"));
    assert!(auto_eat_events(&mut rx).is_empty());
    assert!(!bot.auto_eat().is_eating());
}

#[tokio::test]
async fn eat_emits_start_then_finish_on_consume_end() {
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut rx = bot.events();
    let eating = tokio::spawn({
        let bot = bot.clone();
        async move { bot.auto_eat().eat(EatOpts::default()).await }
    });
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
    assert!(bot.auto_eat().is_eating());

    // the bridge's real consume-finish (entity status 9 / held-stack
    // change) fires `eat_notify`; simulate the wake directly
    bot.windows(|t| t.eat_notify.notify_waiters());

    eating.await.unwrap().unwrap();
    assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
    assert!(!bot.auto_eat().is_eating());
}

#[tokio::test]
async fn eat_timeout_errors_and_finishes_without_fail_under_strict() {
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
    // upstream's timeout Error message, verbatim
    assert_eq!(
        err.to_string(),
        "Eating timed out with a time of 50 milliseconds!"
    );
    // strict mode: the throw precedes upstream's eatFail emit —
    // only eatFinish (the finally) is observable
    assert_eq!(auto_eat_events(&mut rx), ["eatStart", "eatFinish"]);
}

#[tokio::test]
async fn strict_errors_off_still_reports_but_resolves_ok() {
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    opts.strict_errors = false;
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    bot.auto_eat().eat(EatOpts::default()).await.unwrap();
    assert_eq!(
        auto_eat_events(&mut rx),
        ["eatStart", "eatFail", "eatFinish"]
    );
}

#[tokio::test]
async fn second_eat_errors_and_cancel_eat_fails_the_first() {
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut rx = bot.events();
    let eating = tokio::spawn({
        let bot = bot.clone();
        async move { bot.auto_eat().eat(EatOpts::default()).await }
    });
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

    let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
    assert_eq!(err.to_string(), "Already eating!");

    bot.auto_eat().cancel_eat();
    let err = eating.await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "Eating manually canceled!");
    // strict mode suppresses eatFail; the finally's eatFinish stays
    assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
    assert!(!bot.auto_eat().is_eating());

    // no eat in flight → a silent no-op
    bot.auto_eat().cancel_eat();
}

#[tokio::test]
async fn explicit_food_selection_bypasses_scoring_and_ban() {
    // rotten flesh is banned by default but an explicit selection
    // still uses it (upstream `normalizeFoodChoice` has no ban check)
    let bot = eating_bot(&[(36, ItemKind::RottenFlesh), (37, ItemKind::CookedBeef)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    let _ = bot
        .auto_eat()
        .eat(EatOpts {
            food: Some(FoodSelection::Name("rotten_flesh".into())),
            ..EatOpts::default()
        })
        .await;
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::RottenFlesh);
}

#[tokio::test]
async fn monitor_eats_when_hungry_and_reports_each_attempt() {
    let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    bot.auto_eat().enable_auto();
    assert!(bot.auto_eat().enabled());
    send_tick(&bot.client).await;

    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
    // the attempt then fails on the scaffold (no finish packet) —
    // strict mode suppresses eatFail but the finally's eatFinish arrives
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
}

#[tokio::test]
async fn monitor_idles_when_full_and_healthy() {
    let (bot, _shutdown) = monitored_bot(20, 20., &[(36, ItemKind::Apple)]);
    let mut rx = bot.events();
    bot.auto_eat().enable_auto();
    send_tick(&bot.client).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(auto_eat_events(&mut rx).is_empty());
}

#[tokio::test]
async fn monitor_eats_below_min_health_even_when_full() {
    // upstream: `bot.food < minHunger || bot.health < minHealth`
    let (bot, _shutdown) = monitored_bot(20, 10., &[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    bot.auto_eat().enable_auto();
    send_tick(&bot.client).await;
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
}

#[tokio::test]
async fn disable_stops_the_monitor() {
    let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
    bot.auto_eat().enable_auto();
    bot.auto_eat().disable_auto();
    assert!(!bot.auto_eat().enabled());

    let mut rx = bot.events();
    // the receiver may already be gone once the driver sees the
    // cancel flag, so sends are allowed to fail here
    let _ = tick_sender(&bot.client).send(());
    let _ = tick_sender(&bot.client).send(());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(auto_eat_events(&mut rx).is_empty());
}

/// Lagged tolerance: the capacity-1 tick broadcast turns two queued
/// ticks into `Lagged(1)` — a driver that treated `Err` as fatal
/// would exit before the surviving tick drives the eat.
#[tokio::test]
async fn driver_survives_lagged_ticks() {
    let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let cancel = Arc::new(AtomicBool::new(false));
    let _driver = tokio::spawn(drive(bot.clone(), cancel, bot.root.clone()));
    let mut events = bot.events();

    // once the driver is parked on recv, overflow the capacity-1
    // ring — the sends land back-to-back while the current-thread
    // runtime keeps the driver parked, so its next recv is Lagged
    send_tick(&bot.client).await;
    tick_sender(&bot.client).send(()).unwrap();
    tick_sender(&bot.client).send(()).unwrap();

    assert_eq!(next_eat_start(&mut events).await, ItemKind::Apple);
}

/// The bridge's shutdown sender dropping (disconnect) is the only
/// exit signal — the tick channel can never close while the driver's
/// own `Client` holds the sender.
#[tokio::test]
async fn driver_exits_on_shutdown() {
    let (bot, shutdown_tx) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
    let cancel = Arc::new(AtomicBool::new(false));
    let driver = tokio::spawn(drive(bot.clone(), cancel.clone(), bot.root.clone()));

    // parked on ticks like a live connection — and cancel alone
    // can't be observed until a tick or shutdown arrives
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(shutdown_tx);

    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("driver outlived the connection")
        .unwrap();

    // and cancel retires it on the next tick
    let (bot, _tx) = monitored_bot(20, 20., &[]);
    let cancel = Arc::new(AtomicBool::new(false));
    let driver = tokio::spawn(drive(bot.clone(), cancel.clone(), bot.root.clone()));
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.store(true, atomic::Ordering::Relaxed);
    send_tick(&bot.client).await;
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("cancelled driver kept running")
        .unwrap();
}

/// An `Item` like `window.slot(i)` returns — `find_best_choices`
/// takes caller-built lists, upstream `findBestChoices(items, p)`.
fn slot_item(slot: i16, kind: ItemKind) -> Item {
    Item::of(ItemStack::new(kind, 1), slot).unwrap()
}

#[test]
fn find_best_choices_sorts_by_priority_and_skips_banned() {
    let bot = eating_bot(&[]);
    let items = vec![
        slot_item(9, ItemKind::Apple),
        slot_item(10, ItemKind::CookedBeef),
        slot_item(11, ItemKind::RottenFlesh), // banned by default
        slot_item(12, ItemKind::Stone),       // not a food
        slot_item(13, ItemKind::GoldenCarrot),
    ];
    let kinds = |priority: FoodPriority| -> Vec<ItemKind> {
        bot.auto_eat()
            .find_best_choices(&items, priority)
            .iter()
            .map(Item::kind)
            .collect()
    };
    assert_eq!(
        kinds(FoodPriority::FoodPoints),
        [
            ItemKind::CookedBeef,
            ItemKind::GoldenCarrot,
            ItemKind::Apple
        ]
    );
    // the saturation ranking flips the carrot above the beef
    assert_eq!(
        kinds(FoodPriority::Saturation),
        [
            ItemKind::GoldenCarrot,
            ItemKind::CookedBeef,
            ItemKind::Apple
        ]
    );
    // a caller-set banned list is honored too
    let mut opts = bot.auto_eat().opts();
    opts.banned_food.push("cooked_beef".into());
    bot.auto_eat().set_opts(opts);
    assert_eq!(
        kinds(FoodPriority::FoodPoints),
        [ItemKind::GoldenCarrot, ItemKind::Apple]
    );
}

#[test]
fn offhand_slot_45_is_scanned_for_food() {
    // upstream getAllItems covers the offhand — food only in slot
    // 45 must be reachable by the fallback and explicit selection
    let bot = eating_bot(&[(45, ItemKind::CookedBeef)]);
    let auto_eat = bot.auto_eat();
    assert_eq!(
        auto_eat
            .best_food(FoodPriority::FoodPoints)
            .map(|i| (i.kind(), i.slot)),
        Some((ItemKind::CookedBeef, 45))
    );
    assert_eq!(
        auto_eat
            .resolve_food(
                Some(&FoodSelection::Kind(ItemKind::CookedBeef)),
                FoodPriority::FoodPoints
            )
            .map(|i| i.slot),
        Some(45)
    );
    assert_eq!(
        auto_eat
            .resolve_food(
                Some(&FoodSelection::Name("cooked_beef".into())),
                FoodPriority::FoodPoints
            )
            .map(|i| i.slot),
        Some(45)
    );
}

#[tokio::test]
async fn eat_offhand_uses_the_offhand_stack() {
    // the canonical upstream case: `offhand: true` + food already in
    // the offhand → activateItem(true) with nothing to equip
    let bot = eating_bot(&[(45, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_millis(50);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    let eating = tokio::spawn({
        let bot = bot.clone();
        async move {
            bot.auto_eat()
                .eat(EatOpts {
                    offhand: Some(true),
                    ..EatOpts::default()
                })
                .await
        }
    });
    let offhand = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let BotEvent::EatStart { offhand, .. } = rx.recv().await.unwrap() {
                break offhand;
            }
        }
    })
    .await
    .expect("no EatStart within 5s");
    assert!(offhand);
    // the scaffold can't finish an eat — it times out
    eating.await.unwrap().unwrap_err();
}

#[tokio::test]
async fn held_item_type_switch_fails_the_eat() {
    // upstream itemListener: a different-kind stack landing in the
    // food's slot mid-eat rejects `Item switched early to: <name>!`
    // rather than counting as the consume-finish wake
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_secs(5);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    let eating = tokio::spawn({
        let bot = bot.clone();
        async move { bot.auto_eat().eat(EatOpts::default()).await }
    });
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

    // swap the held stack for stone and fire the consume wake
    {
        let mut ecs = bot.client.ecs.write();
        let mut inv = ecs.get_mut::<Inventory>(bot.client.entity).unwrap();
        *inv.inventory_menu.slot_mut(36).unwrap() = ItemStack::new(ItemKind::Stone, 1);
    }
    bot.windows(|t| t.eat_notify.notify_waiters());

    let err = eating.await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "Item switched early to: stone!");
    // strict: eatFail stays unemitted, the finally's eatFinish fires
    assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
}

#[tokio::test]
async fn superseding_consume_reports_distinctly() {
    // a generation bump without the cancel flag — what a second
    // consume() does — must not be mislabeled a manual cancellation
    let bot = eating_bot(&[(36, ItemKind::Apple)]);
    let mut opts = bot.auto_eat().opts();
    opts.eating_timeout = Duration::from_secs(5);
    bot.auto_eat().set_opts(opts);

    let mut rx = bot.events();
    let eating = tokio::spawn({
        let bot = bot.clone();
        async move { bot.auto_eat().eat(EatOpts::default()).await }
    });
    assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

    bot.start_eat();
    let err = eating.await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "Eating cancelled due to calling bot.consume() again"
    );
    assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
}
