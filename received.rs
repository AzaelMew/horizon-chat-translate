// Adds the [t] button at the end of the line.
fn translate_received(overlay: bool, line: &mut horizon_client::text::Text) {
    crate::game::chat_translate::received(overlay, line);
}
