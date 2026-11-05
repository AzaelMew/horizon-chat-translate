# Chat Translate for Horizon

A native mod for [Horizon](https://github.com/horizon-rust/horizon) that translates chat with Google's free web translate endpoint. No API key.

- A dark grey **`[t]`** is added to the end of every chat line. Click it and the translation appears **directly under that line**, even if newer messages have arrived since.
- **`/trans <language> <message>`** translates your message into another language and sends it as chat. The message is in your language (see `/chattr lang`, English by default). The language can be a code (`fi`, `pl`, `zh-CN`) or an English name (`finnish`, `german`, ...).
- **`/chattr lang <code>`** sets your language: `[t]` translates chat into it, and `/trans` takes it as the language you write in (default `en`). It is saved in `config/chat_translate.json`. The source language is detected automatically.

Requests only happen when you click `[t]` or run `/trans`. They run on a background thread and never block the game thread.

## Install

1. Download `chat_translate-0.3.2.hnmod` from the [latest release](../../releases/latest).
2. Put it in your game folder's `horizon-native-mods/` (on Linux: `~/.local/share/horizon/horizon-native-mods/`).
3. Make sure `mod.chat_translate:true` is in `horizon-mods.txt` (Options > Mods shows a Chat Translate row).
4. Build the game with it: `horizon-launcher native-build --instance <instance> --trust-native-code --allow-version-mismatch`, then restart Horizon.

Native mods are compiled into the game and run with your user's rights, with no sandbox. Read the source first; it is small (`runtime.rs` is nearly all of it).

## Requirements

- Pinned to Horizon commit [`83ad384`](https://github.com/horizon-rust/horizon/commit/83ad384f48d0742746d518737511882680aab5b7). It needs the `client_chat_received` hook (hooks API 403 or later), which the 2026-11-04 nightly (`6475f62`, API 359) does not have.
- The launcher builds other native mods you have installed on that same commit. Mods pinned to older commits may need repinning.
- Tested only by compiling against `83ad384`, a unit test of the placement logic on the game's real chat code, and live calls to Google. Placement and scrolling in a running game are lightly tested; please report problems.

## How it works

This is a mixin bundle ([horizon-mixins](https://github.com/horizon-rust/horizon-mixins)), not a trait mod, because inserting a line under another one needs the chat's private message list.

| File | Role |
| --- | --- |
| `chat_insert.rs` | Adds `Chat::translate_insert` to `horizon-client-render`'s chat: a message inserted right under the one carrying the button's click command |
| `runtime.rs` | State, the Google request, the commands, the `[t]` button |
| `received.rs`, `command.rs`, `commands.rs`, `tick.rs`, `list.rs` | One-line hooks into `chat_received`, `native_command`, `native_commands`, `native_tick` and `native_mods` |
| `horizon-mixin.toml` | The manifest |

The game thread never waits on the network. A worker thread asks Google first and only then takes a short lock to hand the result over; the tick uses `try_lock`.

## Limits

- The endpoint is **unofficial and rate limited** (it answers HTTP 429 after fairly light use from one IP; blocks last minutes to hours). Failures show as `Translate failed: ...` in chat and can be retried later by clicking `[t]` again. There is no throttling or fallback service.
- Each request is capped at 1500 characters.
- Lines older than the last 100 messages cannot take a translation under them; it goes to the bottom of the chat.
- Anything typed through `/trans` is sent to Google.

## Build

The launcher builds it. Packaging a `.hnmod` by hand: a zip holding `horizon-native.json` (`{"schema":1,"id":"chat_translate","version":"0.3.2","game":"26.3","horizon":"<40-hex commit>","mixins":"<horizon-mixins commit>"}`) and `mixins/chat_translate/` with the files in this repository. `horizon_revision` in `horizon-mixin.toml` must equal `horizon` in the JSON.

## License

MIT
