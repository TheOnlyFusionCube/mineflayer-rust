//! Many presence sessions in one process, driven by JSON lines: requests on
//! stdin, events on stdout. An owner in another language (a bot runner)
//! keeps its own control plane and hands this process only the
//! connections. The process exits when stdin closes, ending every session.
//!
//! Requests (`id` is the owner's key for one connection attempt; a new
//! attempt takes a new id):
//!
//! ```text
//! {"op":"connect","id":"b1#7","host":"play.example.net","port":25565,"username":"Steve",
//!  "address":"203.0.113.5:25565","protocol":774,"viewDistance":2,
//!  "auth":{"accessToken":"…","uuid":"…"} | null,
//!  "proxy":{"kind":"socks5","address":"198.51.100.2:1080","username":"u","password":"p"} | null,
//!  "client":{"locale":"en_us","chatMode":1,"chatColors":false,"skinParts":0,"rightHanded":true,
//!            "textFiltering":false,"serverListing":false,"particles":2,"brand":"vanilla"},
//!  "resourcePacks":"decline","followTransfers":false,"signedChatOnly":true}
//! {"op":"chat","id":"b1#7","text":"/afk"}
//! {"op":"swing","id":"b1#7"}  {"op":"rotate","id":"b1#7","yaw":12.5,"pitch":-3}
//! {"op":"sneak","id":"b1#7","down":true}  {"op":"quit","id":"b1#7"}
//! ```
//!
//! Events: `hello` (once, with the supported protocols), `status`
//! (`connecting`, `online`, `ended` with an `end`), `chat`, `health`,
//! `notice`, and `error` for a request that could not be applied.

use std::{
    collections::HashMap,
    net::SocketAddr,
    time::Duration,
};

use mineflayer_presence::{
    Auth, AuthServers, ChatKind, ClientInfo, Command, End, Event, Handle, Options, PackPolicy, Proxy, ProxyKind,
    Status, dialects, spawn,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

/// Lines waiting for stdout. Status lines wait for room; chat, health and
/// notice lines are dropped when it is full, so a stalled owner cannot
/// stall the sessions.
const OUT_QUEUE: usize = 65_536;

fn str_field<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    v[name].as_str().ok_or_else(|| format!("missing string field {name}"))
}

fn options(v: &Value) -> Result<Options, String> {
    let port = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()).ok_or("missing or bad port")?;
    let mut o = Options::new(str_field(v, "host")?, port, str_field(v, "username")?);
    if let Some(a) = v["address"].as_str() {
        o.address = Some(a.parse::<SocketAddr>().map_err(|e| format!("bad address {a}: {e}"))?);
    }
    o.protocol = v["protocol"].as_i64().and_then(|p| i32::try_from(p).ok()).filter(|p| *p > 0);
    if let Some(d) = v["viewDistance"].as_u64() {
        o.view_distance = u8::try_from(d.clamp(2, 32)).unwrap_or(2);
    }
    if v["auth"].is_object() {
        let uuid = str_field(&v["auth"], "uuid")?.parse().map_err(|e| format!("bad auth.uuid: {e}"))?;
        o.auth = Auth::Online {
            access_token: str_field(&v["auth"], "accessToken")?.to_owned(),
            uuid,
            servers: AuthServers::default(),
        };
    }
    if v["proxy"].is_object() {
        let p = &v["proxy"];
        let kind = match str_field(p, "kind")?.to_ascii_lowercase().as_str() {
            "socks5" => ProxyKind::Socks5,
            "socks4" => ProxyKind::Socks4,
            "http" => ProxyKind::Http,
            other => return Err(format!("unknown proxy kind {other}")),
        };
        let credentials = p["username"].as_str().map(|u| (u.to_owned(), p["password"].as_str().unwrap_or("").to_owned()));
        o.proxy = Some(Proxy { kind, address: str_field(p, "address")?.to_owned(), credentials });
    }
    if v["client"].is_object() {
        let c = &v["client"];
        let d = ClientInfo::default();
        let small = |name: &str, default: u8| c[name].as_u64().and_then(|n| u8::try_from(n).ok()).unwrap_or(default);
        o.client = ClientInfo {
            locale: c["locale"].as_str().unwrap_or(&d.locale).to_owned(),
            chat_mode: small("chatMode", d.chat_mode),
            chat_colors: c["chatColors"].as_bool().unwrap_or(d.chat_colors),
            skin_parts: small("skinParts", d.skin_parts),
            right_handed: c["rightHanded"].as_bool().unwrap_or(d.right_handed),
            text_filtering: c["textFiltering"].as_bool().unwrap_or(d.text_filtering),
            server_listing: c["serverListing"].as_bool().unwrap_or(d.server_listing),
            particles: small("particles", d.particles),
            brand: c["brand"].as_str().unwrap_or(&d.brand).to_owned(),
        };
    }
    o.resource_packs = match v["resourcePacks"].as_str() {
        Some("decline") => PackPolicy::Decline,
        _ => PackPolicy::Load,
    };
    // fail closed: an owner that leaves it out vets transfer targets itself
    o.follow_transfers = v["followTransfers"].as_bool().unwrap_or(false);
    o.signed_chat_only = v["signedChatOnly"].as_bool().unwrap_or(false);
    Ok(o)
}

fn command(v: &Value) -> Result<Command, String> {
    Ok(match str_field(v, "op")? {
        "chat" => Command::Chat(str_field(v, "text")?.to_owned()),
        "swing" => Command::Swing,
        "rotate" => Command::Rotate {
            yaw: v["yaw"].as_f64().ok_or("missing yaw")? as f32,
            pitch: v["pitch"].as_f64().ok_or("missing pitch")? as f32,
        },
        "sneak" => Command::Sneak(v["down"].as_bool().ok_or("missing down")?),
        "quit" => Command::Quit,
        other => return Err(format!("unknown op {other}")),
    })
}

fn status_line(id: &str, status: &Status) -> Value {
    match status {
        Status::Connecting => json!({ "ev": "status", "id": id, "state": "connecting" }),
        Status::Online { protocol } => json!({ "ev": "status", "id": id, "state": "online", "protocol": protocol }),
        Status::Ended(end) => {
            let end = match end {
                End::Kicked(reason) => json!({ "kind": "kicked", "reason": reason }),
                End::Failed(reason) => json!({ "kind": "failed", "reason": reason }),
                End::Quit => json!({ "kind": "quit" }),
                End::Transfer { host, port } => json!({ "kind": "transfer", "host": host, "port": port }),
            };
            json!({ "ev": "status", "id": id, "state": "ended", "end": end })
        }
    }
}

fn event_line(id: &str, event: &Event) -> Value {
    match event {
        Event::Chat(line) => {
            let kind = match line.kind {
                ChatKind::Player => "player",
                ChatKind::System => "system",
                ChatKind::ActionBar => "actionbar",
            };
            json!({ "ev": "chat", "id": id, "kind": kind, "sender": line.sender, "text": line.text })
        }
        Event::Health(h) => json!({ "ev": "health", "id": id, "health": h }),
        Event::Notice(text) => json!({ "ev": "notice", "id": id, "text": text }),
    }
}

/// Relay one session's status changes and events until it ends, then tell
/// the main loop to forget it.
async fn forward(id: String, handle: Handle, mut events: mpsc::Receiver<Event>, out: mpsc::Sender<String>, done: mpsc::UnboundedSender<String>) {
    let mut status = handle.status();
    loop {
        let current = status.borrow_and_update().clone();
        if out.send(status_line(&id, &current).to_string()).await.is_err() {
            break;
        }
        if matches!(current, Status::Ended(_)) {
            break;
        }
        loop {
            tokio::select! {
                changed = status.changed() => {
                    if changed.is_err() {
                        let _ = done.send(id);
                        return;
                    }
                    break;
                }
                event = events.recv() => match event {
                    Some(e) => {
                        let _ = out.try_send(event_line(&id, &e).to_string());
                    }
                    None => {
                        // the session ended; its final status follows
                        let _ = status.changed().await;
                        break;
                    }
                },
            }
        }
    }
    let _ = done.send(id);
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let (out_tx, mut out_rx) = mpsc::channel::<String>(OUT_QUEUE);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.write_all(b"\n").await.is_err() {
                return;
            }
            if out_rx.is_empty() && stdout.flush().await.is_err() {
                return;
            }
        }
        let _ = stdout.flush().await;
    });
    let protocols: Vec<i32> = dialects().iter().map(|d| d.protocol).collect();
    let _ = out_tx
        .send(json!({ "ev": "hello", "protocols": protocols, "version": env!("CARGO_PKG_VERSION") }).to_string())
        .await;

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<String>();
    let mut sessions: HashMap<String, Handle> = HashMap::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let request: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = out_tx.send(json!({ "ev": "error", "message": format!("bad json: {e}") }).to_string()).await;
                        continue;
                    }
                };
                let id = request["id"].as_str().unwrap_or("").to_owned();
                let result = if id.is_empty() {
                    Err("missing id".to_owned())
                } else if request["op"] == "connect" {
                    if sessions.contains_key(&id) {
                        Err(format!("session {id} exists; a new attempt needs a new id"))
                    } else {
                        options(&request).map(|o| {
                            let (handle, events) = spawn(o);
                            tokio::spawn(forward(id.clone(), handle.clone(), events, out_tx.clone(), done_tx.clone()));
                            sessions.insert(id.clone(), handle);
                        })
                    }
                } else {
                    match (command(&request), sessions.get(&id)) {
                        (Ok(c), Some(handle)) => {
                            // a full command queue means the session is stuck; drop
                            // rather than block every other session's requests
                            let handle = handle.clone();
                            tokio::spawn(async move {
                                let _ = tokio::time::timeout(Duration::from_secs(5), handle.send(c)).await;
                            });
                            Ok(())
                        }
                        (Ok(_), None) => Err(format!("no session {id}")),
                        (Err(e), _) => Err(e),
                    }
                };
                if let Err(message) = result {
                    let _ = out_tx.send(json!({ "ev": "error", "id": id, "message": message }).to_string()).await;
                }
            }
            Some(id) = done_rx.recv() => {
                sessions.remove(&id);
            }
        }
    }

    // stdin closed: the owner is gone, so every session ends
    for handle in sessions.values() {
        let _ = tokio::time::timeout(Duration::from_secs(1), handle.send(Command::Quit)).await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !sessions.is_empty() && tokio::time::Instant::now() < deadline {
        if let Ok(Some(id)) = tokio::time::timeout_at(deadline, done_rx.recv()).await {
            sessions.remove(&id);
        }
    }
    drop(out_tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_connect_request_maps_to_options() {
        let v: Value = serde_json::from_str(
            r#"{"op":"connect","id":"b#1","host":"play.example.net","port":25565,"username":"Steve",
                "address":"203.0.113.5:25565","protocol":774,"viewDistance":3,
                "auth":{"accessToken":"t","uuid":"78ab2cd4-01a8-3913-89a5-d150ec722d26"},
                "proxy":{"kind":"socks5","address":"198.51.100.2:1080","username":"u","password":"p:@x"},
                "client":{"locale":"de_de","chatMode":1,"chatColors":false,"skinParts":0,"rightHanded":true,
                          "textFiltering":false,"serverListing":false,"particles":2,"brand":"vanilla"},
                "resourcePacks":"decline","followTransfers":false,"signedChatOnly":true}"#,
        )
        .unwrap();
        let o = options(&v).unwrap();
        assert_eq!(o.address, Some("203.0.113.5:25565".parse().unwrap()));
        assert_eq!((o.protocol, o.view_distance, o.port), (Some(774), 3, 25565));
        assert!(matches!(o.auth, Auth::Online { .. }));
        let proxy = o.proxy.unwrap();
        assert_eq!((proxy.kind, proxy.credentials), (ProxyKind::Socks5, Some(("u".into(), "p:@x".into()))));
        assert_eq!((o.client.locale.as_str(), o.client.chat_mode, o.client.server_listing), ("de_de", 1, false));
        assert_eq!(o.resource_packs, PackPolicy::Decline);
        assert!(!o.follow_transfers && o.signed_chat_only);
    }

    #[test]
    fn bad_requests_are_errors_not_panics() {
        for bad in [r#"{"op":"connect","id":"x","port":1}"#, r#"{"op":"connect","id":"x","host":"h","port":99999,"username":"u"}"#] {
            assert!(options(&serde_json::from_str(bad).unwrap()).is_err(), "{bad}");
        }
        assert!(command(&json!({"op": "rotate", "id": "x"})).is_err());
        assert!(command(&json!({"op": "fly", "id": "x"})).is_err());
        assert!(matches!(command(&json!({"op": "sneak", "id": "x", "down": true})), Ok(Command::Sneak(true))));
    }

    #[test]
    fn ends_and_chat_lines_serialize_for_the_owner() {
        let transfer = status_line("b#1", &Status::Ended(End::Transfer { host: "h".into(), port: 25566 }));
        assert_eq!(transfer["end"], json!({ "kind": "transfer", "host": "h", "port": 25566 }));
        let chat = event_line(
            "b#1",
            &Event::Chat(mineflayer_presence::ChatLine { kind: ChatKind::ActionBar, sender: String::new(), text: "hi".into() }),
        );
        assert_eq!((chat["kind"].as_str(), chat["text"].as_str()), (Some("actionbar"), Some("hi")));
    }
}
