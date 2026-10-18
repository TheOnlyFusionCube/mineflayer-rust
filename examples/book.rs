//! Write pages into a book and quill, read them back, or toss the book.
//! Port of mineflayer's `examples/book.js`.
//!
//! ```sh
//! cargo run --example book -- <host> <port> [<name>]
//! ```
//! Chat: `write`, `print`, `toss`.

mod common;

use futures::StreamExt;
use mineflayer::{engine::inventory::components::WritableBookContent, prelude::*};

const PAGES: [&str; 4] = [
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
    "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.",
    "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.",
    "Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.",
];

#[tokio::main]
async fn main() {
    let bot = common::connect("book", "book").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() {
            continue;
        }
        let book = bot.find_inventory_item(ItemKind::WritableBook);
        match (message.as_str(), book) {
            (_, None) => {
                let _ = bot.chat("I don't have a book.");
            }
            ("write", Some(book)) => {
                // each word in its own color, every second one bold
                let pages = PAGES
                    .iter()
                    .map(|page| {
                        page.split(' ')
                            .enumerate()
                            .map(|(i, word)| {
                                let bold = if i % 2 == 1 { "§l" } else { "" };
                                format!("§{:x}{bold}{word}", i % 13 + 1)
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect();
                match bot.write_book(i32::from(book.slot), pages).await {
                    Ok(()) => print(&bot),
                    Err(err) => {
                        let _ = bot.chat(&err.to_string());
                    }
                }
            }
            ("print", Some(_)) => print(&bot),
            ("toss", Some(book)) => {
                let _ = bot.toss_stack(&book).await;
            }
            _ => {}
        }
    }
}

fn print(bot: &Bot) {
    let Some(book) = bot.find_inventory_item(ItemKind::WritableBook) else {
        return;
    };
    let Some(content) = book.stack.get_component::<WritableBookContent>() else {
        let _ = bot.chat("The book is empty.");
        return;
    };
    for (i, page) in content.pages.iter().enumerate() {
        let text: String = strip_codes(&page.raw);
        let _ = bot.chat(&format!("Page {}: {text}", i + 1));
    }
}

/// The page without its `§x` formatting codes.
fn strip_codes(page: &str) -> String {
    let mut out = String::new();
    let mut chars = page.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}
