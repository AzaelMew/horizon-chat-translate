// /chattr and /trans, before the server and Horizon's own commands see them.
fn translate_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::chat_translate::command(game, command, ran);
}
