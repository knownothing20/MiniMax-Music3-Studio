//! Naming a track the user did not name.
//!
//! ACE-Step Studio picks the first line of the lyrics that is not a section
//! marker, trims it to a title's length, and falls back to the first few words
//! of the style when there are no lyrics at all. That is what a listener would
//! call the song themselves, so the library reads like a library instead of a
//! column of "Untitled". The same rule is used here, with the section markers
//! MiniMax writes and the caption in place of the style field.

/// The longest a borrowed lyric line may be before it is cut.
const MAX_CHARS: usize = 40;

/// A title for a request that carries none.
pub fn auto_title(caption: &str, lyrics: &str, instrumental: bool) -> String {
    if !instrumental {
        if let Some(line) = first_sung_line(lyrics) {
            return shorten(line);
        }
    }
    let words: Vec<&str> = caption.split_whitespace().take(4).collect();
    if words.is_empty() {
        return "Untitled".to_string();
    }
    capitalise(&words.join(" "))
}

/// The first line that is words rather than a `[chorus]`-style marker.
fn first_sung_line(lyrics: &str) -> Option<&str> {
    lyrics
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !(line.starts_with('[') && line.ends_with(']')))
}

/// Cuts on a character boundary - lyrics are as often Cyrillic as Latin.
fn shorten(line: &str) -> String {
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_CHARS).collect();
    format!("{}…", cut.trim_end())
}

fn capitalise(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrows_the_first_sung_line() {
        let lyrics = "[verse]\nНеон дрожит над мокрым городом\nвторая строка";
        assert_eq!(auto_title("dream pop", lyrics, false), "Неон дрожит над мокрым городом");
    }

    #[test]
    fn cuts_a_long_line_on_a_character_boundary() {
        let lyrics = "Я иду по улице под дождём и думаю о том, что было вчера";
        let title = auto_title("", lyrics, false);
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), MAX_CHARS + 1);
    }

    #[test]
    fn an_instrumental_is_named_after_its_style() {
        assert_eq!(auto_title("symphonic metal with choir and strings", "ignored", true), "Symphonic metal with choir");
    }

    #[test]
    fn nothing_to_go_on() {
        assert_eq!(auto_title("", "[intro]\n[outro]", false), "Untitled");
    }
}
