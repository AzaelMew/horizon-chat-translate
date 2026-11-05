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
use std::time::{Duration, Instant};

use crate::game::Game;
use horizon_client::text::{ClickEvent, Style, Text};

const ID: &str = "chat_translate";
const VERSION: &str = "0.4.1";
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
            let (a, b) = (parts.next(), parts.next());
            match (a, b) {
                (Some("lang"), Some(code)) => set_language(game, code),
                (Some("lang"), None) => {
                    let code = config().target;
                    game.native_feedback(note(format!(
                        "Translating into '{code}'. /{COMMAND} lang <code> changes it."
                    )));
                }
                (Some("key"), Some("clear")) => set_key(game, None),
                (Some("key"), Some(key)) => set_key(game, Some(key)),
                (Some("key"), None) => key_status(game),
                (Some("first"), Some(which)) => set_first(game, which),
                (Some("first"), None) => key_status(game),
                (Some(id), _) => match id.parse::<u64>() {
                    Ok(id) => translate(game, id),
                    Err(_) => game.native_feedback(note(USAGE)),
                },
                _ => game.native_feedback(note(USAGE)),
            }
        }
        _ => {}
    }
}

const USAGE: &str = "/chattr lang <code> | key <deepl key> | key clear | first google|deepl";

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
                let (chat, font) = (&mut game.ui.chat, &game.gui.atlas.font);
                // The line left the chat's history: the bottom is the best place left.
                if !chat.translate_insert(font, &marker(id), text.clone()) {
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

/// What the config file holds (`config/chat_translate.json`).
struct Config {
    /// Your language: `[t]` translates into it, `/trans` takes it as the language you write in.
    target: String,
    /// A DeepL API key, the fallback when Google refuses.
    deepl_key: Option<String>,
    /// Try DeepL before Google.
    deepl_first: bool,
}

fn config_path() -> Option<std::path::PathBuf> {
    Game::native_config_dir()
        .ok()
        .map(|dir| dir.join("chat_translate.json"))
}

fn read_json() -> serde_json::Map<String, serde_json::Value> {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Read on the game thread; the threads that call the services get a copy.
fn config() -> Config {
    let json = read_json();
    Config {
        target: json
            .get("target")
            .and_then(|v| v.as_str())
            .filter(|code| valid_language(code))
            .unwrap_or("en")
            .to_owned(),
        deepl_key: json
            .get("deepl_key")
            .and_then(|v| v.as_str())
            .filter(|key| valid_key(key))
            .map(str::to_owned),
        deepl_first: json.get("first").and_then(|v| v.as_str()) == Some("deepl"),
    }
}

/// Changes the config file and keeps the rest of it. It holds the DeepL key, so it is made
/// readable by its owner only.
fn update_config(
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "no config folder".to_owned())?;
    let mut json = read_json();
    change(&mut json);
    std::fs::write(&path, serde_json::Value::Object(json).to_string()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn valid_language(code: &str) -> bool {
    (2..=8).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_key(key: &str) -> bool {
    (10..=100).contains(&key.len())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '.' | '_'))
}

fn set_language(game: &mut Game<'_>, code: &str) {
    if !valid_language(code) {
        game.native_feedback(error(format!(
            "'{code}' is not a language code (en, fi, pl, de, zh-CN...)"
        )));
        return;
    }
    match update_config(|json| {
        json.insert("target".into(), code.into());
    }) {
        Ok(()) => {
            game.native_feedback(note(format!("Translating into '{code}' from now on.")));
            // Translations already shown are into the old language; a click may redo them.
            with_state(|s| s.busy.clear());
        }
        Err(e) => game.native_feedback(error(format!("Could not save the language: {e}"))),
    }
}

/// `/chattr key <key>` saves the DeepL key; `/chattr key clear` removes it. The key is never
/// printed back whole, and the typed command is kept out of the chat history (see `history.rs`).
fn set_key(game: &mut Game<'_>, key: Option<&str>) {
    if let Some(key) = key
        && !valid_key(key)
    {
        return game.native_feedback(error("That doesn't look like a DeepL key (10-100 letters, digits, - : . _)."));
    }
    let saved = update_config(|json| match key {
        Some(key) => {
            json.insert("deepl_key".into(), key.into());
        }
        None => {
            json.remove("deepl_key");
        }
    });
    match (saved, key) {
        (Ok(()), Some(key)) => game.native_feedback(note(format!(
            "DeepL key saved (ends in {}). Google is tried first, DeepL when Google refuses.",
            mask(key)
        ))),
        (Ok(()), None) => game.native_feedback(note("DeepL key removed. Google only.")),
        (Err(e), _) => game.native_feedback(error(format!("Could not save the key: {e}"))),
    }
}

fn mask(key: &str) -> String {
    let n = key.chars().count();
    key.chars().skip(n.saturating_sub(4)).collect()
}

fn set_first(game: &mut Game<'_>, which: &str) {
    let which = which.to_ascii_lowercase();
    if which != "google" && which != "deepl" {
        return game.native_feedback(note("/chattr first google|deepl"));
    }
    match update_config(|json| {
        json.insert("first".into(), which.clone().into());
    }) {
        Ok(()) => game.native_feedback(note(format!("{which} is tried first from now on."))),
        Err(e) => game.native_feedback(error(format!("Could not save that: {e}"))),
    }
}

fn key_status(game: &mut Game<'_>) {
    let cfg = config();
    let line = match &cfg.deepl_key {
        Some(key) => format!(
            "DeepL key set (ends in {}). Order: {}.",
            mask(key),
            if cfg.deepl_first { "DeepL, then Google" } else { "Google, then DeepL" }
        ),
        None => "No DeepL key: Google only. /chattr key <key> adds one.".to_owned(),
    };
    game.native_feedback(note(line));
}

/// A click on a line's [t].
fn translate(game: &mut Game<'_>, id: u64) {
    let cfg = config();
    let target = cfg.target.clone();
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
            let result = translate_text(&text, "auto", &target, &cfg);
            with_state(|s| match result {
                Ok(r) if r.from.eq_ignore_ascii_case(&target) || r.to.trim() == text.trim() => {
                    s.done.push(Done::Under(id, note(format!("[{}] Already in '{target}'.", r.from))));
                }
                Ok(r) => {
                    let via = if r.service == "google" { String::new() } else { format!(" {}", r.service) };
                    s.done.push(Done::Under(
                        id,
                        Text::literal(format!("[{}>{target}{via}] ", r.from))
                            .with_color(GRAY)
                            .append(Text::literal(r.to).with_color(GREEN)),
                    ));
                }
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
    let cfg = config();
    let mine = cfg.target.clone();
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
            let result = translate_text(&message, &mine, &code, &cfg);
            with_state(|s| match result {
                Ok(r) => s.outbox.push(r.to),
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

/// A finished translation: the detected language, the text and who made it.
struct Reply {
    from: String,
    to: String,
    service: &'static str,
}

/// How long Google is left alone after it answered 429: every click would wait for an answer
/// that is going to be a refusal.
const GOOGLE_PAUSE: Duration = Duration::from_secs(600);

static GOOGLE_PAUSED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

fn google_pause_left() -> Option<Duration> {
    let until = (*GOOGLE_PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner()))?;
    until.checked_duration_since(Instant::now())
}

/// Google, DeepL, or both in the configured order. Runs on a thread of its own.
fn translate_text(text: &str, source: &str, target: &str, cfg: &Config) -> Result<Reply, String> {
    let deepl_ready = cfg.deepl_key.is_some();
    let order: &[&str] = match (deepl_ready, cfg.deepl_first) {
        (true, true) => &["deepl", "google"],
        (true, false) => &["google", "deepl"],
        (false, _) => &["google"],
    };
    let mut failures = Vec::new();
    for service in order {
        let result = match *service {
            "google" => match google_pause_left() {
                Some(left) => Err(format!(
                    "Google is rate limited (429), paused for {} more min",
                    left.as_secs().div_ceil(60)
                )),
                None => google(text, source, target),
            },
            _ => deepl(cfg.deepl_key.as_deref().unwrap_or_default(), text, source, target),
        };
        match result {
            Ok(reply) => return Ok(reply),
            Err(e) => failures.push(e),
        }
    }
    if !deepl_ready {
        failures.push("add a DeepL key as a fallback: /chattr key <key>".into());
    }
    Err(failures.join("; "))
}

/// Google's unauthenticated web endpoint.
fn google(text: &str, source: &str, target: &str) -> Result<Reply, String> {
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
        .map_err(|e| match e {
            ureq::Error::Status(429, _) => {
                *GOOGLE_PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Instant::now() + GOOGLE_PAUSE);
                "Google rate limited (429)".to_owned()
            }
            ureq::Error::Status(code, _) => format!("Google answered {code}"),
            ureq::Error::Transport(_) => "Google could not be reached".to_owned(),
        })?;
    let body = response.into_string().map_err(|e| e.to_string())?;
    let (from, to) = parse_google(&body)?;
    Ok(Reply { from, to, service: "google" })
}

/// Google's reply: `[[["translated","original",..],..],null,"detected"..]`.
fn parse_google(body: &str) -> Result<(String, String), String> {
    let body: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
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

/// DeepL's API: the free plan's key ends in `:fx` and uses its own host.
fn deepl(key: &str, text: &str, source: &str, target: &str) -> Result<Reply, String> {
    let to = deepl_target(target).ok_or_else(|| format!("DeepL doesn't translate into '{target}'"))?;
    let mut request = serde_json::json!({ "text": [text], "target_lang": to });
    if source != "auto" {
        let from = deepl_source(source).ok_or_else(|| format!("DeepL doesn't read '{source}'"))?;
        request["source_lang"] = from.into();
    }
    let host = if key.ends_with(":fx") { "https://api-free.deepl.com" } else { "https://api.deepl.com" };
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build()
        .post(&format!("{host}/v2/translate"))
        .set("Authorization", &format!("DeepL-Auth-Key {key}"))
        .set("Content-Type", "application/json")
        .send_string(&request.to_string())
        .map_err(|e| match e {
            ureq::Error::Status(403, _) => "DeepL rejected the key (403)".to_owned(),
            ureq::Error::Status(429, _) => "DeepL rate limited (429)".to_owned(),
            ureq::Error::Status(456, _) => "DeepL monthly quota used up (456)".to_owned(),
            ureq::Error::Status(code, _) => format!("DeepL answered {code}"),
            ureq::Error::Transport(_) => "DeepL could not be reached".to_owned(),
        })?;
    let body = response.into_string().map_err(|e| e.to_string())?;
    let (from, to) = parse_deepl(&body)?;
    Ok(Reply { from, to, service: "deepl" })
}

/// DeepL's reply: `{"translations":[{"detected_source_language":"RU","text":"..."}]}`.
fn parse_deepl(body: &str) -> Result<(String, String), String> {
    let body: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let first = body["translations"].get(0).ok_or("unexpected reply from DeepL")?;
    let to = first["text"].as_str().filter(|t| !t.is_empty()).ok_or("empty reply from DeepL")?;
    let from = first["detected_source_language"].as_str().unwrap_or("?").to_ascii_lowercase();
    Ok((from, to.to_owned()))
}

/// DeepL's target codes keep the variant (`EN-US`, `PT-BR`, `ZH-HANS`); Google's don't.
fn deepl_target(code: &str) -> Option<String> {
    const OTHER: &[&str] = &[
        "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hu", "id", "it", "ja", "ko",
        "lt", "lv", "nl", "pl", "ro", "ru", "sk", "sl", "sv", "tr", "uk",
    ];
    let lower = code.to_ascii_lowercase();
    let mapped = match lower.as_str() {
        "en" | "en-us" => "EN-US",
        "en-gb" => "EN-GB",
        "pt" | "pt-br" => "PT-BR",
        "pt-pt" => "PT-PT",
        "zh" | "zh-cn" | "zh-hans" => "ZH-HANS",
        "zh-tw" | "zh-hant" => "ZH-HANT",
        "no" | "nb" => "NB",
        other if OTHER.contains(&other) => return Some(other.to_ascii_uppercase()),
        _ => return None,
    };
    Some(mapped.to_owned())
}

/// DeepL's source codes have no variants (`EN`, `PT`, `ZH`).
fn deepl_source(code: &str) -> Option<String> {
    let base = code.split('-').next()?.to_ascii_lowercase();
    let base = if base == "no" { "nb".to_owned() } else { base };
    const SOURCES: &[&str] = &[
        "ar", "bg", "cs", "da", "de", "el", "en", "es", "et", "fi", "fr", "hu", "id", "it", "ja",
        "ko", "lt", "lv", "nb", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk", "zh",
    ];
    SOURCES.contains(&base.as_str()).then(|| base.to_ascii_uppercase())
}

/// `/chattr key <something>`: a command line the history must not keep.
pub fn is_secret(message: &str) -> bool {
    let mut parts = message.trim_start_matches('/').split_whitespace();
    parts.next() == Some(COMMAND) && parts.next() == Some("key") && parts.next().is_some()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_reply_is_read() {
        let body = r#"[[["Hello, ","Hola, ",null,null,10],["how are you?","¿cómo estás?",null,null,10]],null,"es"]"#;
        assert_eq!(parse_google(body).unwrap(), ("es".into(), "Hello, how are you?".into()));
        assert!(parse_google("<html>Sorry").is_err());
    }

    #[test]
    fn deepl_reply_is_read() {
        let body = r#"{"translations":[{"detected_source_language":"RU","text":"cat says meow"}]}"#;
        assert_eq!(parse_deepl(body).unwrap(), ("ru".into(), "cat says meow".into()));
        assert!(parse_deepl(r#"{"translations":[]}"#).is_err());
    }

    #[test]
    fn deepl_codes_follow_deepls_rules() {
        assert_eq!(deepl_target("en").as_deref(), Some("EN-US"));
        assert_eq!(deepl_target("ru").as_deref(), Some("RU"));
        assert_eq!(deepl_target("zh-CN").as_deref(), Some("ZH-HANS"));
        assert_eq!(deepl_target("pt").as_deref(), Some("PT-BR"));
        assert_eq!(deepl_target("no").as_deref(), Some("NB"));
        assert_eq!(deepl_target("sm"), None);
        assert_eq!(deepl_source("en-GB").as_deref(), Some("EN"));
        assert_eq!(deepl_source("zh-CN").as_deref(), Some("ZH"));
        assert_eq!(deepl_source("hi"), None);
    }

    #[test]
    fn keys_and_secret_commands() {
        assert!(valid_key("279a2e9d-83b3-c416-7e2d-f721593e42a0:fx"));
        assert!(!valid_key("short") && !valid_key("has space in it ok") && !valid_key("a;b;c;d;e;f;g;h"));
        assert_eq!(mask("279a2e9d:fx"), "d:fx");
        assert!(is_secret("/chattr key abc123abc123"));
        assert!(!is_secret("/chattr key") && !is_secret("/chattr lang fi") && !is_secret("/trans fi hello"));
    }

    #[test]
    fn google_alone_without_a_key_and_the_order_with_one() {
        // No key: only Google is tried, and the failure says how to add DeepL.
        let cfg = Config { target: "en".into(), deepl_key: None, deepl_first: false };
        *GOOGLE_PAUSED_UNTIL.lock().unwrap() = Some(Instant::now() + Duration::from_secs(300));
        let e = translate_text("hola", "auto", "en", &cfg).err().unwrap();
        assert!(e.contains("paused") && e.contains("/chattr key"), "{e}");
        // With an unusable key DeepL is tried after Google and both reasons are shown.
        let cfg = Config { target: "sm".into(), deepl_key: Some("0123456789abcdef:fx".into()), deepl_first: false };
        let e = translate_text("hola", "auto", "sm", &cfg).err().unwrap();
        assert!(e.contains("paused") && e.contains("DeepL doesn't translate into 'sm'"), "{e}");
        *GOOGLE_PAUSED_UNTIL.lock().unwrap() = None;
    }

    #[test]
    fn player_name_is_not_translated() {
        assert_eq!(translatable("<Azael> hola amigo").as_deref(), Some("hola amigo"));
        assert_eq!(translatable("<Azael> :)"), None);
    }
}
