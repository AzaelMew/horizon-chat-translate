// `Chat::translate_insert`. This module sits inside `chat`, so it sees the chat's private
// `messages`; the messages are newest first, so "under" a message is its own index.
impl super::Chat {
    /// Shows `content` as a message directly under the one holding a click event that runs
    /// `marker`. False when that message has left the history (the caller shows it at the bottom).
    pub fn translate_insert(
        &mut self,
        font: &crate::font::Font,
        marker: &str,
        content: super::Text,
        ticks: u64,
    ) -> bool {
        let Some(at) = self.messages.iter().position(|m| {
            m.content.chars().any(|(_, style)| {
                matches!(&style.click, Some(super::ClickEvent::RunCommand(c)) if c == marker)
            })
        }) else {
            return false;
        };
        let source = self.messages[at].source.clone();
        self.messages.insert(
            at,
            super::Message {
                added: ticks,
                content,
                tag: None,
                source,
                signature: None,
            },
        );
        self.messages.truncate(100);
        let scroll = self.scroll;
        self.rewrap(font);
        self.scroll = scroll.min(self.lines.len());
        true
    }
}
