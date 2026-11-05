fn translate_commands(commands: &mut Vec<&'static str>) {
    crate::game::chat_translate::commands(commands);
}
