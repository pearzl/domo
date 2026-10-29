//! Audit event messages can embed full agent output. List responses expose a
//! bounded preview; the original stored message stays untouched and is served
//! by the single event endpoint.

/// Preview length in characters (not bytes) so CJK and ASCII behave alike.
pub const PREVIEW_CHARS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPreview {
    pub message: String,
    pub message_length: usize,
    pub message_truncated: bool,
}

pub fn preview(message: &str) -> EventPreview {
    let message_length = message.chars().count();
    if message_length <= PREVIEW_CHARS {
        return EventPreview {
            message: message.to_string(),
            message_length,
            message_truncated: false,
        };
    }
    EventPreview {
        message: message.chars().take(PREVIEW_CHARS).collect(),
        message_length,
        message_truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_below_limit_is_untouched() {
        let source = "job finished in 3s";
        let preview = preview(source);
        assert_eq!(preview.message, source);
        assert_eq!(preview.message_length, 18);
        assert!(!preview.message_truncated);
    }

    #[test]
    fn ascii_over_limit_truncates_by_character() {
        let source = "a".repeat(PREVIEW_CHARS + 25);
        let preview = preview(&source);
        assert_eq!(preview.message.chars().count(), PREVIEW_CHARS);
        assert_eq!(preview.message_length, PREVIEW_CHARS + 25);
        assert!(preview.message_truncated);
        assert_eq!(preview.message.len(), PREVIEW_CHARS);
    }

    #[test]
    fn chinese_over_limit_truncates_by_character_not_byte() {
        let source = "审计事件".repeat(300);
        let preview = preview(&source);
        assert_eq!(preview.message.chars().count(), PREVIEW_CHARS);
        assert_eq!(preview.message_length, 1200);
        assert!(preview.message_truncated);
        assert_eq!(preview.message, "审计事件".repeat(250));
    }

    #[test]
    fn mixed_text_over_limit_keeps_character_boundary() {
        let source = format!("{}{}", "a".repeat(600), "事件".repeat(300));
        let preview = preview(&source);
        assert_eq!(preview.message.chars().count(), PREVIEW_CHARS);
        assert_eq!(preview.message_length, 1200);
        assert!(preview.message_truncated);
        assert!(preview.message.starts_with(&"a".repeat(600)));
        assert!(preview.message.ends_with('件'));
    }

    #[test]
    fn exact_limit_is_not_truncated() {
        let source = "界".repeat(PREVIEW_CHARS);
        let preview = preview(&source);
        assert_eq!(preview.message, source);
        assert_eq!(preview.message_length, PREVIEW_CHARS);
        assert!(!preview.message_truncated);
    }

    #[test]
    fn empty_message_stays_empty() {
        let preview = preview("");
        assert_eq!(preview.message, "");
        assert_eq!(preview.message_length, 0);
        assert!(!preview.message_truncated);
    }
}
