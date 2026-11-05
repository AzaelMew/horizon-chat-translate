//! Chat Translate: every chat line gets a clickable `[t]` at its end. Clicking it runs
//! the client command `/chattr <id>`; the line is translated on a background thread with
//! Google's free web endpoint and the result joins the chat as a new line.
//!
//! Commands: `/chattr lang <code>` sets the target language (default `en`, kept in
//! `config/chat_translate.json`). `/trans <language> <message>` translates an English message
//! into the language and sends it to the server as chat.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use horizon_client::mod_api::{
    ClientChatReceivedEvent, CommandEvent, CommandsEvent, Flow, Hooks, Host, ModsEvent, NativeMod,
    Text, TickEvent,
};
use horizon_client::text::{ClickEvent, Style};

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(Mod)
}

struct Mod;

/// The command a button runs; typed by hand it sets the language.
const COMMAND: &str = "chattr";
/// `/trans <language> <message>`: English in, the language out, sent as chat.
const SEND_COMMAND: &str = "trans";
/// Lines remembered for their button. A button on an older line says it expired.
const KEEP: usize = 400;
/// Longest text sent to Google in one request.
const MAX_CHARS: usize = 1500;

const GRAY: u32 = 0xAAAAAA;
const DARK_GRAY: u32 = 0x555555;
const GREEN: u32 = 0x55FF55;
const RED: u32 = 0xFF5555;

#[derive(Default)]
struct State {
    next_id: u64,
    /// id -> text to translate, oldest first in `order`.
    lines: HashMap<u64, String>,
    order: VecDeque<u64>,
    /// Finished lines waiting for the game thread: the chat is only written from `tick`.
    done: Vec<Text>,
    /// Translated messages `/trans` will send, also from `tick`.
    outbox: Vec<String>,
    /// Ids being fetched, so a double click asks once.
    busy: Vec<u64>,
    /// Finished translations, so a second click costs nothing.
    cache: HashMap<u64, Text>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// For the game thread: None when another thread holds the state, so a tick never waits.
fn try_with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = match STATE.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    Some(f(guard.get_or_insert_with(State::default)))
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

impl Hooks for Mod {
    /// A row in Options > Mods. Without it the game forgets the mod's `mod.chat_translate:true`
    /// line in `horizon-mods.txt` the next time it saves the list, and the mod drops out of the
    /// next build.
    fn mods(&self, _host: &mut dyn Host, event: &mut ModsEvent<'_>) {
        event.mods.push(NativeMod {
            id: "chat_translate".into(),
            title: format!("Chat Translate {}", env!("CARGO_PKG_VERSION")),
            description: "A [t] button on chat lines translates them with Google. /trans <language> <message> sends your English message translated.".into(),
            keys: Vec::new(),
        });
    }

    fn commands(&self, _host: &mut dyn Host, event: &mut CommandsEvent<'_>) {
        event.commands.push(COMMAND);
        event.commands.push(SEND_COMMAND);
    }

    /// `Flow::Cancel` says the mod ran the command, so the server never sees it.
    fn command(&self, host: &mut dyn Host, event: &mut CommandEvent<'_>) -> Flow {
        let mut parts = event.command.split_whitespace();
        let name = parts.next();
        if name == Some(SEND_COMMAND) {
            send_translated(host, event.command[SEND_COMMAND.len()..].trim());
            return Flow::Cancel;
        }
        if name != Some(COMMAND) {
            return Flow::Continue;
        }
        match (parts.next(), parts.next()) {
            (Some("lang"), Some(code)) => set_language(host, code),
            (Some("lang"), None) => {
                let code = language(host);
                host.feedback(note(format!("Translating into '{code}'. /{COMMAND} lang <code> changes it.")));
            }
            (Some(id), _) => match id.parse::<u64>() {
                Ok(id) => translate(host, id),
                Err(_) => host.feedback(note(format!("Usage: /{COMMAND} lang <code>"))),
            },
            _ => host.feedback(note(format!("Usage: /{COMMAND} lang <code>"))),
        }
        Flow::Cancel
    }

    fn client_chat_received(
        &self,
        _host: &mut dyn Host,
        event: &mut ClientChatReceivedEvent<'_>,
    ) -> Flow {
        // The action bar is not chat; a line with nothing to read has nothing to translate.
        if event.overlay {
            return Flow::Continue;
        }
        let Some(text) = translatable(&event.message.plain()) else {
            return Flow::Continue;
        };
        let id = with_state(|s| {
            let id = s.next_id;
            s.next_id += 1;
            s.lines.insert(id, text);
            s.order.push_back(id);
            while s.order.len() > KEEP {
                if let Some(old) = s.order.pop_front() {
                    s.lines.remove(&old);
                    s.cache.remove(&old);
                }
            }
            id
        });
        let button = Text::styled(
            "[t]",
            Style {
                color: Some(DARK_GRAY),
                click: Some(ClickEvent::RunCommand(format!("/{COMMAND} {id}"))),
                hover: Some(Box::new(Text::literal("Click to translate this line"))),
                ..Style::default()
            },
        );
        let line = std::mem::replace(event.message, Text::empty());
        *event.message = line.append(Text::literal(" ")).append(button);
        Flow::Continue
    }

    /// Writes finished translations into the chat, on the game thread.
    fn tick(&self, host: &mut dyn Host, _event: &mut TickEvent) {
        let Some((done, outbox)) =
            try_with_state(|s| (std::mem::take(&mut s.done), std::mem::take(&mut s.outbox)))
        else {
            return;
        };
        for line in done {
            host.feedback(line);
        }
        for message in outbox {
            if let Err(e) = host.send_chat(&message) {
                host.feedback(Text::literal(format!("Could not send: {e}")).with_color(RED));
            }
        }
    }
}

/// The part of a chat line worth translating: without a leading `<name> `, and not empty.
fn translatable(plain: &str) -> Option<String> {
    let mut text = plain.trim();
    if let Some(rest) = text.strip_prefix('<')
        && let Some(end) = rest.find("> ")
    {
        text = rest[end + 2..].trim_start();
    }
    if !text.chars().any(char::is_alphabetic) {
        return None;
    }
    Some(text.chars().take(MAX_CHARS).collect())
}

fn note(message: String) -> Text {
    Text::literal(message).with_color(GRAY)
}

fn config_path(host: &mut dyn Host) -> Option<std::path::PathBuf> {
    host.config_dir().ok().map(|dir| dir.join("chat_translate.json"))
}

fn language(host: &mut dyn Host) -> String {
    config_path(host)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["target"].as_str().map(str::to_owned))
        .filter(|code| valid_language(code))
        .unwrap_or_else(|| "en".to_owned())
}

fn valid_language(code: &str) -> bool {
    (2..=8).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn set_language(host: &mut dyn Host, code: &str) {
    if !valid_language(code) {
        host.feedback(Text::literal(format!("'{code}' is not a language code (en, fi, pl, de, zh-CN...)")).with_color(RED));
        return;
    }
    let json = serde_json::json!({ "target": code }).to_string();
    let saved = config_path(host)
        .ok_or_else(|| "no config folder".to_owned())
        .and_then(|path| std::fs::write(path, json).map_err(|e| e.to_string()));
    match saved {
        Ok(()) => host.feedback(note(format!("Translating into '{code}' from now on."))),
        Err(e) => host.feedback(Text::literal(format!("Could not save the language: {e}")).with_color(RED)),
    }
    // A translation already made is into the old language.
    with_state(|s| s.cache.clear());
}

fn translate(host: &mut dyn Host, id: u64) {
    let target = language(host);
    let step = with_state(|s| {
        if let Some(done) = s.cache.get(&id) {
            return Err(Some(done.clone()));
        }
        let Some(text) = s.lines.get(&id).cloned() else {
            return Err(None);
        };
        if s.busy.contains(&id) {
            return Err(Some(note("Still translating...".into())));
        }
        s.busy.push(id);
        Ok(text)
    });
    let text = match step {
        Ok(text) => text,
        Err(Some(line)) => return host.feedback(line),
        Err(None) => return host.feedback(note("That line is too old to translate.".into())),
    };
    let spawned = std::thread::Builder::new()
        .name("chat-translate".into())
        .spawn(move || {
            let line = match fetch(&text, "auto", &target) {
                Ok((from, to)) if from.eq_ignore_ascii_case(&target) || to.trim() == text.trim() => {
                    note(format!("[{from}] Already in '{target}'."))
                }
                Ok((from, to)) => Text::literal(format!("[{from}>{target}] "))
                    .with_color(GRAY)
                    .append(Text::literal(to).with_color(GREEN)),
                Err(e) => Text::literal(format!("Translate failed: {e}")).with_color(RED),
            };
            with_state(|s| {
                s.busy.retain(|b| *b != id);
                if !line.plain().starts_with("Translate failed") {
                    s.cache.insert(id, line.clone());
                }
                s.done.push(line);
            });
        });
    if spawned.is_err() {
        with_state(|s| s.busy.retain(|b| *b != id));
        host.feedback(Text::literal("Could not start the translation thread.").with_color(RED));
    }
}

/// `/trans <language> <message>`: the message is English; its translation is sent as chat.
fn send_translated(host: &mut dyn Host, args: &str) {
    fn usage(host: &mut dyn Host) {
        host.feedback(note(format!("Usage: /{SEND_COMMAND} <language> <message>  (English in)")));
    }
    let Some((language, message)) = args.split_once(char::is_whitespace) else {
        return usage(host);
    };
    let message = message.trim().chars().take(MAX_CHARS).collect::<String>();
    let Some(code) = language_code(language) else {
        return host.feedback(Text::literal(format!("'{language}' is not a language (en, fi, pl, de, french...)")).with_color(RED));
    };
    if message.is_empty() {
        return usage(host);
    }
    let spawned = std::thread::Builder::new()
        .name("chat-translate-send".into())
        .spawn(move || {
            // The request first, the lock after: the game thread takes the same lock every tick.
            let result = fetch(&message, "en", &code);
            with_state(|s| match result {
                Ok((_, translated)) => s.outbox.push(translated),
                Err(e) => s.done.push(Text::literal(format!("Translate failed: {e}")).with_color(RED)),
            });
        });
    if spawned.is_err() {
        host.feedback(Text::literal("Could not start the translation thread.").with_color(RED));
    }
}

/// A language code (`fi`, `zh-CN`) or an English name (`finnish`) as Google's code.
fn language_code(word: &str) -> Option<String> {
    const NAMES: &[(&str, &str)] = &[
        ("arabic", "ar"), ("chinese", "zh-CN"), ("czech", "cs"), ("danish", "da"), ("dutch", "nl"),
        ("english", "en"), ("estonian", "et"), ("finnish", "fi"), ("french", "fr"), ("german", "de"),
        ("greek", "el"), ("hindi", "hi"), ("hungarian", "hu"), ("indonesian", "id"), ("italian", "it"),
        ("japanese", "ja"), ("korean", "ko"), ("norwegian", "no"), ("polish", "pl"),
        ("portuguese", "pt"), ("romanian", "ro"), ("russian", "ru"), ("spanish", "es"),
        ("swedish", "sv"), ("turkish", "tr"), ("ukrainian", "uk"), ("vietnamese", "vi"),
    ];
    let lower = word.to_ascii_lowercase();
    if let Some((_, code)) = NAMES.iter().find(|(name, _)| *name == lower) {
        return Some((*code).to_owned());
    }
    valid_language(word).then(|| word.to_owned())
}

/// Google's unauthenticated web endpoint. Returns (detected language, translation).
fn fetch(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build()
        .get("https://translate.googleapis.com/translate_a/single")
        .query("client", "gtx")
        .query("sl", source)
        .query("tl", target)
        .query("dt", "t")
        .query("q", text)
        .call()
        .map_err(|e| e.to_string())?;
    let body = response.into_string().map_err(|e| e.to_string())?;
    let body: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let translated: String = body[0]
        .as_array()
        .ok_or("unexpected reply")?
        .iter()
        .filter_map(|segment| segment[0].as_str())
        .collect();
    if translated.is_empty() {
        return Err("empty reply".into());
    }
    let from = body[2].as_str().unwrap_or("?").to_owned();
    Ok((from, translated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_player_name_is_not_translated() {
        assert_eq!(translatable("<Azael> hola amigo").as_deref(), Some("hola amigo"));
        assert_eq!(translatable("Server: hola").as_deref(), Some("Server: hola"));
        assert_eq!(translatable("<Azael> :)"), None);
        assert_eq!(translatable("   "), None);
    }

    #[test]
    fn language_codes_are_checked() {
        assert!(valid_language("en") && valid_language("zh-CN"));
        assert!(!valid_language("") && !valid_language("e") && !valid_language("en&q=x"));
    }

    #[test]
    fn language_names_and_codes_resolve() {
        assert_eq!(language_code("Finnish").as_deref(), Some("fi"));
        assert_eq!(language_code("pl").as_deref(), Some("pl"));
        assert_eq!(language_code("zh-CN").as_deref(), Some("zh-CN"));
        assert_eq!(language_code("not a language!"), None);
    }

    #[test]
    fn english_goes_out_in_the_asked_language() {
        let (_, to) = fetch("Hello, how are you?", "en", "fi").unwrap();
        assert!(to.to_lowercase().contains("hei"), "{to}");
    }

    /// Real request to Google's endpoint.
    #[test]
    fn google_translates_and_detects_the_language() {
        let (from, to) = fetch("Hola, ¿cómo estás? Me llamo Azael.", "auto", "en").unwrap();
        assert_eq!(from, "es");
        assert!(to.to_lowercase().contains("how are you"), "{to}");
    }
}
