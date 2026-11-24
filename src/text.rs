pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

pub fn shorten(text: &str, max: usize) -> String {
    if char_len(text) <= max { text.to_owned() } else { format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

pub const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_len_counts_characters_not_bytes() {
        assert_eq!(char_len(""), 0);
        assert_eq!(char_len("abc"), 3);
        assert_eq!(char_len("äöü🤖"), 4);
    }

    #[test]
    fn shorten_keeps_short_text() {
        assert_eq!(shorten("hello", 5), "hello");
        assert_eq!(shorten("", 3), "");
    }

    #[test]
    fn shorten_cuts_with_ellipsis_within_max() {
        assert_eq!(shorten("hello world", 6), "hello…");
        assert_eq!(char_len(&shorten("hello world", 6)), 6);
    }

    #[test]
    fn shorten_cuts_on_character_boundaries() {
        assert_eq!(shorten("ääääää", 4), "äää…");
        assert_eq!(shorten("🤖🤖🤖", 2), "🤖…");
    }

    #[test]
    fn shorten_to_zero_does_not_panic() {
        assert_eq!(shorten("abc", 0), "…");
    }

    #[test]
    fn plural_picks_by_count() {
        assert_eq!(plural(0, "match", "matches"), "matches");
        assert_eq!(plural(1, "match", "matches"), "match");
        assert_eq!(plural(2, "match", "matches"), "matches");
    }
}
