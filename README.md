# Chat Translate for Horizon

A native mod for [Horizon](https://github.com/horizon-rust/horizon) that translates chat with Google's free web translate endpoint. No API key.

- A dark grey **`[t]`** is added to the end of every chat line. Click it and the translation appears **directly under that line**, even if newer messages have arrived since.
- **`/trans <language> <message>`** translates your message into another language and sends it as chat. The message is in your language (see `/chattr lang`, English by default). The language can be a code (`fi`, `pl`, `zh-CN`) or an English name (`finnish`, `german`, ...).
- **`/chattr lang <code>`** sets your language: `[t]` translates chat into it, and `/trans` takes it as the language you write in (default `en`). It is saved in `config/chat_translate.json`. The source language is detected automatically.

Translation goes to Google's free endpoint first. If Google refuses (it rate limits quickly) and you have added a [DeepL](https://www.deepl.com/pro-api) API key, DeepL takes over. Requests only happen when you click `[t]` or run `/trans`. They run on a background thread and never block the game thread.

## DeepL fallback (optional)

Google's free endpoint is unofficial and starts answering HTTP 429 after fairly light use. A free DeepL key makes translation keep working then, and Google stays the first choice so DeepL's monthly allowance is spent only when Google refuses.

1. Make a free account at https://www.deepl.com/pro-api (the **API Free** plan, 500,000 characters per month). Copy the key; a free key ends in `:fx`.
2. In the game, run `/chattr key <your key>`. It is saved in `config/chat_translate.json` (readable by your user only). The command is run by the mod and is **not** sent to the server and **not** written to the chat history or `command_history.txt`. Anyone watching your screen can still see it while you type it.
3. That's all. `[t]` and `/trans` now go Google, then DeepL.

| Command | What it does |
| --- | --- |
| `/chattr key <key>` | Save the DeepL key |
| `/chattr key` | Show whether a key is set (last 4 characters only) and the order |
| `/chattr key clear` | Remove the key; Google only again |
| `/chattr first deepl` / `first google` | Which service is tried first (default Google) |

After a Google 429 the mod leaves Google alone for 10 minutes and goes straight to DeepL, so a click isn't delayed by a request that would be refused. A line translated by DeepL is labelled, for example `[ru>en deepl]`. DeepL covers fewer languages than Google (about 30, including Russian, Ukrainian, Polish, Finnish and Spanish); for one it doesn't support you get a clear message. DeepL's free plan may use your text to improve its service, see their terms.

## Install

1. Download `chat_translate-0.4.1.hnmod` from the [latest release](../../releases/latest).
2. Put it in your game folder's `horizon-native-mods/` (on Linux: `~/.local/share/horizon/horizon-native-mods/`).
3. Make sure `mod.chat_translate:true` is in `horizon-mods.txt` (Options > Mods shows a Chat Translate row).
4. Build the game with it: `horizon-launcher native-build --instance <instance> --trust-native-code --allow-version-mismatch`, then restart Horizon.

Native mods are compiled into the game and run with your user's rights, with no sandbox. Read the source first; it is small (`runtime.rs` is nearly all of it).

## Requirements

- Pinned to Horizon commit [`66f3981`](https://github.com/horizon-rust/horizon/commit/66f3981f522b24c250b21523a70080965b940424) (Nightly 2026-11-05, `66f3981`). It needs the `client_chat_received` hook, which that commit has. Builds for older nightlies need an older release of this mod.
- The launcher builds other native mods you have installed on that same commit. Mods pinned to older commits may need repinning.
- 0.4.1 adapts the chat insert to that commit's chat, where a message also carries its time, a repeat count and its wrapped line count: a translation takes its line's tick and time (so it fades and is timestamped with it), counts once, and is laid out like any message. A unit test in `chat_insert.rs` runs against the game's real chat code.
- Verified against `66f3981`: the mixins apply, the game crate compiles with all injections, and the chat tests pass. Not tested in a running game, and live Google/DeepL calls were not repeated for this release; please report problems.

## How it works

This is a mixin bundle ([horizon-mixins](https://github.com/horizon-rust/horizon-mixins)), not a trait mod, because inserting a line under another one needs the chat's private message list.

| File | Role |
| --- | --- |
| `chat_insert.rs` | Adds `Chat::translate_insert` to `horizon-client-render`'s chat: a message inserted right under the one carrying the button's click command |
| `runtime.rs` | State, the Google request, the commands, the `[t]` button |
| `received.rs`, `command.rs`, `commands.rs`, `tick.rs`, `list.rs`, `history.rs` | One-line hooks into `chat_received`, `native_command`, `native_commands`, `native_tick`, `native_mods` and `handle_chat_input` (which keeps `/chattr key` out of the history) |
| `horizon-mixin.toml` | The manifest |

The game thread never waits on the network. A worker thread asks Google first and only then takes a short lock to hand the result over; the tick uses `try_lock`.

## Limits

- Google's endpoint is **unofficial and rate limited** (HTTP 429 after fairly light use from one IP; blocks last minutes to hours). Add a DeepL key (above) to keep translating through it. Failures show as `Translate failed: ...` with the reason of each service, and can be retried by clicking `[t]` again.
- Each request is capped at 1500 characters.
- Lines older than the last 100 messages cannot take a translation under them; it goes to the bottom of the chat.
- Anything you translate or send through `/trans` is sent to Google, and to DeepL when it is used.

## Build

The launcher builds it. Packaging a `.hnmod` by hand: a zip holding `horizon-native.json` (`{"schema":1,"id":"chat_translate","version":"0.4.1","game":"26.3","horizon":"<40-hex commit>","mixins":"<horizon-mixins commit>"}`) and `mixins/chat_translate/` with the files in this repository. `horizon_revision` in `horizon-mixin.toml` must equal `horizon` in the JSON.

## License

MIT
