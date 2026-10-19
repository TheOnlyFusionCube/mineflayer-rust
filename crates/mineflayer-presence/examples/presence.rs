//! Live check of the presence client against one server: connect `count`
//! offline bots, keep them online for `seconds`, and from the first bot send
//! a chat line, a line with a colour code, a command, a swing, a rotation and
//! a sneak. Prints the first bot's events and status changes with their time,
//! then a summary line. Exit status 0 only when every bot came online and
//! none ended early.
//!
//! With `--no-read` the chat and health events go unread for the whole run,
//! as with an owner that stalls; the bots must stay online regardless.
//! `--protocol=N` speaks protocol N instead of the one the server reports
//! (a ViaVersion server accepts older and newer clients). `--then=TEXT`
//! has the first bot send TEXT 10 s after it comes online, such as a
//! proxy's `/server b`. `--yggdrasil=URL` logs in online against that
//! authlib-injector API, with token `tok-<name>` (scripts/presence-compat
//! runs a mock of one). `--proxy=URL` connects through a SOCKS5, SOCKS4 or
//! HTTP proxy (`socks5://user:pass@host:port`).
//!
//! ```sh
//! cargo run --release -p mineflayer-presence --example presence -- 127.0.0.1 25565 1 60
//! ```

use std::time::{Duration, Instant};

use mineflayer_presence::{Auth, AuthServers, Command, Options, Status, spawn};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let no_read = a.iter().any(|x| x == "--no-read");
    let flag = |name: &str| a.iter().find_map(|x| x.strip_prefix(&format!("--{name}="))).map(str::to_owned);
    let forced: Option<i32> = flag("protocol").map(|p| p.parse().expect("--protocol"));
    let then = flag("then");
    let yggdrasil = flag("yggdrasil");
    let proxy = flag("proxy").map(|p| mineflayer_presence::Proxy::parse(&p).expect("--proxy=socks5|socks4|http://[user[:pass]@]host:port"));
    let a: Vec<String> = a.into_iter().filter(|x| !x.starts_with("--")).collect();
    if a.len() != 5 {
        eprintln!("usage: presence <host> <port> <count> <seconds> [--no-read]");
        std::process::exit(2);
    }
    let port: u16 = a[2].parse().expect("port");
    let count: usize = a[3].parse().expect("count");
    let seconds: u64 = a[4].parse().expect("seconds");
    let protocol = match forced {
        Some(p) => p,
        None => mineflayer_presence::ping(&a[1], port, proxy.as_ref()).await.expect("ping"),
    };
    let name = mineflayer_presence::dialect(protocol).map_or("unsupported", |d| d.name());
    println!("server protocol {protocol} ({name})");

    let start = Instant::now();
    let mut bots = Vec::new();
    for i in 0..count {
        let username = format!("pres{i:03}");
        let auth = match &yggdrasil {
            None => Auth::Offline,
            Some(root) => Auth::Online {
                access_token: format!("tok-{username}"),
                uuid: mineflayer_crypto::offline::generate_uuid(&username),
                servers: AuthServers::yggdrasil(root),
            },
        };
        let (handle, events) = spawn(Options {
            host: a[1].clone(),
            port,
            username,
            auth,
            proxy: proxy.clone(),
            protocol: Some(protocol),
            view_distance: 2,
        });
        let status = handle.status();
        bots.push((handle, events, status));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut online = vec![false; count];
    let mut last0 = Status::Connecting;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut acted = false;
    let mut online_at: Option<Instant> = None;
    let mut whispered = false;
    while tokio::time::Instant::now() < deadline {
        for (i, (handle, events, status)) in bots.iter_mut().enumerate() {
            let now = status.borrow().clone();
            if matches!(now, Status::Online { .. }) {
                online[i] = true;
            }
            if i == 0 && now != last0 {
                println!("status {:.1}s {now:?}", start.elapsed().as_secs_f64());
                last0 = now;
            }
            if !no_read {
                while let Ok(e) = events.try_recv() {
                    if i == 0 {
                        println!("event {:.1}s {e:?}", start.elapsed().as_secs_f64());
                    }
                }
            }
            // pres001 is a moment behind pres000, so the whisper waits
            if i == 0
                && let Some(t) = online_at
                && !whispered
                && t.elapsed() >= Duration::from_secs(5)
            {
                whispered = true;
                handle.send(Command::Chat("/tell pres001 signed hello".into())).await;
            }
            if i == 0
                && let (Some(t), Some(text)) = (online_at, then.as_ref())
                && t.elapsed() >= Duration::from_secs(10)
            {
                handle.send(Command::Chat(text.clone())).await;
                println!("then {:.1}s {text}", start.elapsed().as_secs_f64());
                online_at = None;
            }
            if i == 0 && online[0] && !acted {
                acted = true;
                online_at = Some(Instant::now());
                handle.send(Command::Chat("presence check".into())).await;
                handle.send(Command::Chat("colour \u{a7}ccode\u{7} stripped".into())).await;
                handle.send(Command::Chat("/help".into())).await;
                handle.send(Command::Swing).await;
                handle.send(Command::Rotate { yaw: 30.0, pitch: -10.0 }).await;
                handle.send(Command::Sneak(true)).await;
                tokio::time::sleep(Duration::from_millis(450)).await;
                handle.send(Command::Sneak(false)).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ended: Vec<String> = bots
        .iter()
        .enumerate()
        .filter_map(|(i, (_, _, status))| match &*status.borrow() {
            Status::Ended(end) => Some(format!("pres{i:03}: {end:?}")),
            _ => None,
        })
        .collect();
    for (handle, _, _) in &bots {
        handle.send(Command::Quit).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let up = online.iter().filter(|o| **o).count();
    println!("summary protocol={protocol} online={up}/{count} ended_early={}", ended.len());
    for e in &ended {
        println!("ended {e}");
    }
    std::process::exit(if up == count && ended.is_empty() { 0 } else { 1 });
}
