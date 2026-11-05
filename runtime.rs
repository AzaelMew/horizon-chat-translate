// Chat Translate's runtime, a module of the game (`crate::game::chat_translate`).
//
// Every chat line gets a dark grey [t]. A click runs the client command `/chattr <id>`; the line
// is translated on a thread of its own with Google's free web endpoint and the translation is
// shown as a message directly under the line. `/trans <language> <message>` translates an
// English message and sends it as chat. The game thread never waits for the network: a thread
// asks Google first and takes the state's lock only to hand the answer over, and the tick only
// `try_lock`s.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use crate::game::Game;
use horizon_client::text::{ClickEvent, Style, Text};

const ID: &str = "chat_translate";
const VERSION: &str = "0.3.1";
/// The button's command; typed by hand, `/chattr lang <code>` sets the target language.
const COMMAND: &str = "chattr";
/// `/trans <language> <message>`: your language in (`/chattr lang`, English by default), the
/// asked language out, sent as chat.
const SEND_COMMAND: &str = "trans";
/// Lines remembered for their button. A button on an older line says it expired.
const KEEP: usize = 400;
/// Longest text sent to Google in one request.
const MAX_CHARS: usize = 1500;

const GRAY: u32 = 0xAAAAAA;
const DARK_GRAY: u32 = 0x555555;
const GREEN: u32 = 0x55FF55;
const RED: u32 = 0xFF5555;

/// What a thread hands the game thread.
enum Done {
    /// The translation of line `id`: shown under it.
    Under(u64, Text),
    /// A line for the bottom of the chat: an error, a note.
    Note(Text),
}

#[derive(Default)]
struct State {
    next_id: u64,
    /// id -> text to translate; `order` is oldest first.
    lines: HashMap<u64, String>,
    order: VecDeque<u64>,
    done: Vec<Done>,
    /// Translated messages `/trans` will send, from the tick.
    outbox: Vec<String>,
    /// Ids being fetched or already shown, so a second click does nothing.
    busy: HashSet<u64>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

/// For the game thread: None when another thread holds the state, so a tick never waits.
fn try_with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = match STATE.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    Some(f(guard.get_or_insert_with(State::default)))
}

fn note(message: impl Into<String>) -> Text {
    Text::literal(message.into()).with_color(GRAY)
}

fn error(message: impl Into<String>) -> Text {
    Text::literal(message.into()).with_color(RED)
}

fn marker(id: u64) -> String {
    format!("/{COMMAND} {id}")
}

/// A row in Options > Mods. Without it the game forgets the mod's line in `horizon-mods.txt`
/// the next time it saves the list, and the mod drops out of the next build.
pub fn listed(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    mods.push(crate::game::native_hooks::NativeMod {
        id: ID.into(),
        title: format!("Chat Translate {VERSION}"),
        description: "A [t] on chat lines translates them with Google, right under the line. /trans <language> <message> sends your English message translated.".into(),
        keys: Vec::new(),
    });
}

pub fn commands(commands: &mut Vec<&'static str>) {
    commands.push(COMMAND);
    commands.push(SEND_COMMAND);
}

/// The [t] at the end of a line (not the action bar).
pub fn received(overlay: bool, line: &mut Text) {
    if overlay {
        return;
    }
    let Some(text) = translatable(&line.plain()) else {
        return;
    };
    let id = with_state(|s| {
        let id = s.next_id;
        s.next_id += 1;
        s.lines.insert(id, text);
        s.order.push_back(id);
        while s.order.len() > KEEP {
            if let Some(old) = s.order.pop_front() {
                s.lines.remove(&old);
                s.busy.remove(&old);
            }
        }
        id
    });
    let button = Text::styled(
        "[t]",
        Style {
            color: Some(DARK_GRAY),
            click: Some(ClickEvent::RunCommand(marker(id))),
            hover: Some(Box::new(Text::literal("Click to translate this line"))),
            ..Style::default()
        },
    );
    let original = std::mem::replace(line, Text::empty());
    *line = original.append(Text::literal(" ")).append(button);
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

/// `/chattr ...` and `/trans ...`. Sets `ran` for them, so the server never sees them.
pub fn command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    let mut parts = command.split_whitespace();
    match parts.next() {
        Some(SEND_COMMAND) => {
            *ran = true;
            send_translated(game, command[SEND_COMMAND.len()..].trim());
        }
        Some(COMMAND) => {
            *ran = true;
            match (parts.next(), parts.next()) {
                (Some("lang"), Some(code)) => set_language(game, code),
                (Some("lang"), None) => {
                    let code = language();
                    game.native_feedback(note(format!(
                        "Translating into '{code}'. /{COMMAND} lang <code> changes it."
                    )));
                }
                (Some(id), _) => match id.parse::<u64>() {
                    Ok(id) => translate(game, id),
                    Err(_) => game.native_feedback(note(format!("Usage: /{COMMAND} lang <code>"))),
                },
                _ => game.native_feedback(note(format!("Usage: /{COMMAND} lang <code>"))),
            }
        }
        _ => {}
    }
}

/// Once a tick, on the game thread: translations into the chat, `/trans` messages out.
pub fn tick(game: &mut Game<'_>) {
    let Some((done, outbox)) =
        try_with_state(|s| (std::mem::take(&mut s.done), std::mem::take(&mut s.outbox)))
    else {
        return;
    };
    for item in done {
        match item {
            Done::Under(id, text) => {
                let (ticks, chat, font) = (game.ticks, &mut game.ui.chat, &game.gui.atlas.font);
                // The line left the chat's history: the bottom is the best place left.
                if !chat.translate_insert(font, &marker(id), text.clone(), ticks) {
                    game.native_feedback(text);
                }
            }
            Done::Note(text) => game.native_feedback(text),
        }
    }
    for message in outbox {
        if let Err(e) = game.native_send_chat(&message) {
            game.native_feedback(error(format!("Could not send: {e}")));
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    Game::native_config_dir()
        .ok()
        .map(|dir| dir.join("chat_translate.json"))
}

fn language() -> String {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["target"].as_str().map(str::to_owned))
        .filter(|code| valid_language(code))
        .unwrap_or_else(|| "en".to_owned())
}

fn valid_language(code: &str) -> bool {
    (2..=8).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn set_language(game: &mut Game<'_>, code: &str) {
    if !valid_language(code) {
        game.native_feedback(error(format!(
            "'{code}' is not a language code (en, fi, pl, de, zh-CN...)"
        )));
        return;
    }
    let json = serde_json::json!({ "target": code }).to_string();
    let saved = config_path()
        .ok_or_else(|| "no config folder".to_owned())
        .and_then(|path| std::fs::write(path, json).map_err(|e| e.to_string()));
    match saved {
        Ok(()) => {
            game.native_feedback(note(format!("Translating into '{code}' from now on.")));
            // Translations already shown are into the old language; a click may redo them.
            with_state(|s| s.busy.clear());
        }
        Err(e) => game.native_feedback(error(format!("Could not save the language: {e}"))),
    }
}

/// A click on a line's [t].
fn translate(game: &mut Game<'_>, id: u64) {
    let target = language();
    let step = with_state(|s| {
        let Some(text) = s.lines.get(&id).cloned() else {
            return Err(Some("That line is too old to translate."));
        };
        // Already being translated, or already shown under its line.
        if !s.busy.insert(id) {
            return Err(None);
        }
        Ok(text)
    });
    let text = match step {
        Ok(text) => text,
        Err(Some(why)) => return game.native_feedback(note(why)),
        Err(None) => return,
    };
    let spawned = std::thread::Builder::new()
        .name("chat-translate".into())
        .spawn(move || {
            // The request first, the lock after: the game thread takes the same lock every tick.
            let result = fetch(&text, "auto", &target);
            with_state(|s| match result {
                Ok((from, to)) if from.eq_ignore_ascii_case(&target) || to.trim() == text.trim() => {
                    s.done.push(Done::Under(id, note(format!("[{from}] Already in '{target}'."))));
                }
                Ok((from, to)) => s.done.push(Done::Under(
                    id,
                    Text::literal(format!("[{from}>{target}] "))
                        .with_color(GRAY)
                        .append(Text::literal(to).with_color(GREEN)),
                )),
                Err(e) => {
                    // A failure may be tried again.
                    s.busy.remove(&id);
                    s.done.push(Done::Note(error(format!("Translate failed: {e}"))));
                }
            });
        });
    if spawned.is_err() {
        with_state(|s| s.busy.remove(&id));
        game.native_feedback(error("Could not start the translation thread."));
    }
}

/// `/trans <language> <message>`: the message is in the language set with `/chattr lang`
/// (English by default); its translation is sent as chat.
fn send_translated(game: &mut Game<'_>, args: &str) {
    let mine = language();
    let usage = format!("Usage: /{SEND_COMMAND} <language> <message>  ({mine} in; change with /{COMMAND} lang <code>)");
    let Some((language, message)) = args.split_once(char::is_whitespace) else {
        return game.native_feedback(note(usage));
    };
    let message = message.trim().chars().take(MAX_CHARS).collect::<String>();
    let Some(code) = language_code(language) else {
        return game.native_feedback(error(format!(
            "'{language}' is not a language (en, fi, pl, de, french...)"
        )));
    };
    if message.is_empty() {
        return game.native_feedback(note(usage));
    }
    let spawned = std::thread::Builder::new()
        .name("chat-translate-send".into())
        .spawn(move || {
            let result = fetch(&message, &mine, &code);
            with_state(|s| match result {
                Ok((_, translated)) => s.outbox.push(translated),
                Err(e) => s.done.push(Done::Note(error(format!("Translate failed: {e}")))),
            });
        });
    if spawned.is_err() {
        game.native_feedback(error("Could not start the translation thread."));
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
