// `Chat::translate_insert`. This module sits inside `chat`, so it sees the chat's private
// `messages`; the messages are newest first, so "under" a message is its own index.
impl super::Chat {
    /// Shows `content` as a message directly under the one holding a click event that runs
    /// `marker`, with that message's tick and time. False when that message has left the history (the caller shows it at the bottom).
    pub fn translate_insert(
        &mut self,
        font: &crate::font::Font,
        marker: &str,
        content: super::Text,
    ) -> bool {
        let Some(at) = self.messages.iter().position(|m| {
            m.content.chars().any(|(_, style)| {
                matches!(&style.click, Some(super::ClickEvent::RunCommand(c)) if c == marker)
            })
        }) else {
            return false;
        };
        // The translation fades with its line and shows its time: it takes the line's tick and
        // clock time, not those of the click. It is one occurrence (no compact-chat count);
        // `rewrap` below lays it out and sets `wrapped`.
        let (source, added, time) = (
            self.messages[at].source.clone(),
            self.messages[at].added,
            self.messages[at].time,
        );
        self.messages.insert(
            at,
            super::Message {
                added,
                time,
                repeats: 1,
                wrapped: 0,
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

#[cfg(test)]
mod tests {
    use super::super::{Chat, ChatQol, ChatSource, ClickEvent, Style, Text};
    use crate::{font::Font, hud::Rect};
    use std::{collections::HashMap, path::Path};

    const MARKER: &str = "/chattr 7";

    fn font() -> Font {
        // The missing-glyph metrics let this run without game assets.
        Font::load(
            Path::new("/nonexistent-horizon-chat-test-resources"),
            &HashMap::new(),
            &[],
            (1, 1),
            (0, 0),
            Rect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
                w: 1,
                h: 1,
            },
        )
    }

    /// A chat line the way `received` leaves it: the text, a space and the `[t]` button.
    fn line(text: &str, marker: &str) -> Text {
        let button = Text::styled(
            "[t]",
            Style {
                click: Some(ClickEvent::RunCommand(marker.to_string())),
                ..Style::default()
            },
        );
        Text::literal(text)
            .append(Text::literal(" "))
            .append(button)
    }

    fn shown(chat: &Chat) -> Vec<String> {
        chat.lines.iter().map(|l| l.content.plain()).collect()
    }

    /// The translation is a message like the one it answers: that message's time and tick, one
    /// occurrence, and exactly the lines it wrapped to. Not the time of the click, not 00:00.
    #[test]
    fn a_translation_takes_the_time_tick_and_lines_of_its_source() {
        let font = font();
        let mut chat = Chat {
            clock: Some((9, 5)),
            ..Chat::default()
        };
        chat.qol = ChatQol {
            timestamps: true,
            compact: true,
        };
        chat.add_message(
            &font,
            line("hello", MARKER),
            None,
            ChatSource::Server,
            7,
            false,
        );
        // The click comes later, at another time, with a newer line already in.
        chat.clock = Some((23, 59));
        chat.add_message(
            &font,
            Text::literal("later"),
            None,
            ChatSource::Server,
            50,
            false,
        );

        assert!(chat.translate_insert(&font, MARKER, Text::literal("hola")));

        assert_eq!(chat.messages.len(), 3);
        let translation = &chat.messages[1];
        assert_eq!(translation.content.plain(), "hola");
        assert_eq!(translation.time, (9, 5), "the source line's time");
        assert_eq!(translation.added, 7, "fades with the source line");
        assert_eq!(translation.repeats, 1);
        assert_eq!(translation.source, ChatSource::Server);
        assert!(translation.signature.is_none());
        assert!(translation.wrapped > 0, "laid out, so it shows");
        assert_eq!(
            chat.messages.iter().map(|m| m.wrapped).sum::<usize>(),
            chat.lines.len(),
            "every message's line count matches the lines in the chat"
        );
        assert_eq!(
            shown(&chat),
            ["[23:59] later", "[09:05] hola", "[09:05] hello [t]"]
        );
    }

    /// A source that compact chat had counted keeps its count; the translation is not counted.
    #[test]
    fn a_translation_under_a_counted_line_is_not_counted() {
        let font = font();
        let mut chat = Chat {
            clock: Some((9, 5)),
            ..Chat::default()
        };
        chat.qol.compact = true;
        for _ in 0..3 {
            chat.add_message(
                &font,
                line("gg", MARKER),
                None,
                ChatSource::Server,
                0,
                false,
            );
        }
        assert_eq!(chat.messages[0].repeats, 3);
        assert!(chat.translate_insert(&font, MARKER, Text::literal("gg!")));
        assert_eq!(chat.messages[0].repeats, 1);
        assert_eq!(chat.messages[1].repeats, 3);
        assert_eq!(shown(&chat), ["gg!", "gg [t] (x3)"]);
    }

    /// A line that is gone from the history takes no translation and changes nothing.
    #[test]
    fn a_missing_line_changes_nothing() {
        let font = font();
        let mut chat = Chat::default();
        chat.add_message(
            &font,
            line("hello", MARKER),
            None,
            ChatSource::Server,
            0,
            false,
        );
        assert!(!chat.translate_insert(&font, "/chattr 8", Text::literal("hola")));
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(shown(&chat), ["hello [t]"]);
    }

    /// A reader scrolled up stays where they were.
    #[test]
    fn the_scroll_survives_the_insert() {
        let font = font();
        let mut chat = Chat::default();
        chat.add_message(
            &font,
            line("hello", MARKER),
            None,
            ChatSource::Server,
            0,
            false,
        );
        for i in 0..5 {
            chat.add_message(
                &font,
                Text::literal(format!("n{i}")),
                None,
                ChatSource::Server,
                0,
                false,
            );
        }
        chat.scroll = 2;
        assert!(chat.translate_insert(&font, MARKER, Text::literal("hola")));
        assert_eq!(chat.scroll, 2);
    }
}
