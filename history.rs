// The chat keeps typed commands in `command_history.txt`. A DeepL key typed into
// `/chattr key ...` must not end up there, or in the recent-messages list: run the command here
// and stop, before `handle_chat_input` adds the line to the history.
fn translate_history(game: &mut crate::game::Game<'_>, message: &str) -> core::ops::ControlFlow<()> {
    if crate::game::chat_translate::is_secret(message) {
        let mut ran = false;
        crate::game::chat_translate::command(game, message.trim().trim_start_matches('/'), &mut ran);
        core::ops::ControlFlow::Break(())
    } else {
        core::ops::ControlFlow::Continue(())
    }
}
