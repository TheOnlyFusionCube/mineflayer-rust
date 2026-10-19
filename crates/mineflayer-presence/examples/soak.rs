//! Soak test: hold `count` offline bots on one server for `minutes`. With
//! `churn_s` above 0, every `churn_s` seconds a tenth of the bots quit and
//! reconnect. Every 60 s it prints one CSV row: elapsed minutes, RSS and
//! peak RSS in KiB (Linux), threads, bots online, unexpected ends so far,
//! and sessions started. An unexpected end is any end the soak did not ask
//! for. Exit status 0 only when there were none.
//!
//! ```sh
//! cargo run --release -p mineflayer-presence --example soak -- 127.0.0.1 25565 500 120 0
//! ```

use std::time::{Duration, Instant};

use mineflayer_presence::{Auth, Command, Handle, Options, Status, spawn};

fn proc_status() -> (u64, u64, u64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (kb("VmRSS:"), kb("VmHWM:"), kb("Threads:"))
}

struct Bot {
    handle: Handle,
    quitting: bool,
}

fn start(host: &str, port: u16, protocol: i32, i: usize) -> Bot {
    let (handle, mut events) = spawn(Options {
        host: host.to_owned(),
        port,
        username: format!("soak{i:03}"),
        auth: Auth::Offline,
        proxy: None,
        protocol: Some(protocol),
        view_distance: 2,
    });
    // an owner that reads its events, as a runner relaying chat would
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    Bot { handle, quitting: false }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 6 {
        eprintln!("usage: soak <host> <port> <count> <minutes> <churn_s>");
        std::process::exit(2);
    }
    let host = a[1].clone();
    let port: u16 = a[2].parse().expect("port");
    let count: usize = a[3].parse().expect("count");
    let minutes: u64 = a[4].parse().expect("minutes");
    let churn = Duration::from_secs(a[5].parse().expect("churn_s"));
    let protocol = mineflayer_presence::ping(&host, port, None).await.expect("ping");

    let start_at = Instant::now();
    let mut bots: Vec<Bot> = Vec::new();
    for i in 0..count {
        bots.push(start(&host, port, protocol, i));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut started = count;
    let mut unexpected = 0usize;
    let mut reasons: Vec<String> = Vec::new();
    let mut next_report = Instant::now();
    let mut next_churn = Instant::now() + churn;
    let mut round = 0usize;
    println!("minute,rss_kb,hwm_kb,threads,online,unexpected,started");
    while start_at.elapsed() < Duration::from_secs(minutes * 60) {
        let mut online = 0;
        for (i, bot) in bots.iter_mut().enumerate() {
            let status = bot.handle.status().borrow().clone();
            match status {
                Status::Online { .. } => online += 1,
                Status::Ended(end) => {
                    if !bot.quitting {
                        unexpected += 1;
                        if reasons.len() < 20 {
                            reasons.push(format!("{:.1}min soak{i:03}: {end:?}", start_at.elapsed().as_secs_f64() / 60.0));
                        }
                    }
                    *bot = start(&host, port, protocol, i);
                    started += 1;
                }
                Status::Connecting => {}
            }
        }
        if !churn.is_zero() && Instant::now() >= next_churn {
            next_churn += churn;
            for (i, bot) in bots.iter_mut().enumerate() {
                if i % 10 == round % 10 {
                    bot.quitting = true;
                    bot.handle.send(Command::Quit).await;
                }
            }
            round += 1;
        }
        if Instant::now() >= next_report {
            next_report += Duration::from_secs(60);
            let (rss, hwm, threads) = proc_status();
            println!(
                "{:.1},{rss},{hwm},{threads},{online},{unexpected},{started}",
                start_at.elapsed().as_secs_f64() / 60.0
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    for r in &reasons {
        println!("unexpected {r}");
    }
    std::process::exit(i32::from(unexpected > 0));
}
