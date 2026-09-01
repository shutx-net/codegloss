//! Cutting a paragraph into the sentences the engine is meant to see.
//!
//! FuguMT is a sentence-level model, and [`CommentShape`] hands it paragraphs:
//! P3 merges a run of `//` lines into one unit so that a sentence spread over
//! three lines reaches the engine whole, which is right, but it also means a
//! five-sentence paragraph arrives as one input. Greedy decoding on such an
//! input drops clauses - fluently, so the reader cannot tell:
//!
//! ```text
//! Returns None once the queue is closed, which happens when the server is shutting down.
//!   whole -> キューが閉じられたら None を返します。          (the clause is gone)
//!   split -> キューが閉じた時点で None を返します。
//!            これはサーバーがシャットダウンしているときに起こります。
//! ```
//!
//! IMPORTANT: this runs on the *masked* text, never on the source. A full stop
//! inside a URL, a `foo.bar()` call or a piece of inline code is a sentence
//! boundary to any rule simple enough to be worth having; by the time
//! [`mask`](crate::mask) has run, every one of those is a placeholder and the
//! only full stops left are punctuation.
//!
//! [`CommentShape`]: crate::CommentShape

/// Characters that can end a sentence.
const TERMINATORS: [char; 3] = ['.', '!', '?'];

/// Words that take a full stop without ending a sentence.
///
/// Deliberately short. A list long enough to be complete would also be long
/// enough to swallow real boundaries, and the cost of the two mistakes is not
/// the same: a missed split leaves the text as it is today, while a wrong split
/// cuts a sentence in half.
const ABBREVIATIONS: [&str; 14] = [
    "e.g", "i.e", "etc", "vs", "cf", "al", "approx", "resp", "dr", "mr", "mrs", "ms", "prof", "st",
];

/// Characters that can open a sentence, on top of an upper-case letter or a
/// digit. A placeholder opens with `X`, so it is covered by the letter rule.
const OPENERS: [char; 7] = ['`', '"', '\'', '(', '[', '{', '*'];

/// Splits masked prose into sentences.
///
/// The pieces are trimmed and carry their own terminator. Text that holds no
/// boundary comes back as a single piece, and text that is all whitespace as
/// none at all.
pub fn split_sentences(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut cursor = 0;

    while cursor < text.len() {
        let character = text[cursor..]
            .chars()
            .next()
            .expect("the cursor is on a character boundary");
        if !TERMINATORS.contains(&character) {
            cursor += character.len_utf8();
            continue;
        }

        // `...` and `?!` end one sentence between them, not three.
        let mut end = cursor;
        while text[end..].starts_with(TERMINATORS) {
            end += 1;
        }

        let rest = &text[end..];
        let next = rest.trim_start();
        // A boundary needs white space after it - `v0.1` and `a.b` are one
        // token - and something that reads as the start of a sentence after
        // that.
        if next.len() == rest.len() || !opens_a_sentence(next) || is_abbreviation(&text[start..end])
        {
            cursor = end;
            continue;
        }

        let piece = text[start..end].trim();
        if !piece.is_empty() {
            pieces.push(piece);
        }
        start = text.len() - next.len();
        cursor = start;
    }

    let tail = text[start..].trim();
    if !tail.is_empty() {
        pieces.push(tail);
    }
    pieces
}

fn opens_a_sentence(rest: &str) -> bool {
    rest.chars().next().is_some_and(|character| {
        character.is_uppercase() || character.is_ascii_digit() || OPENERS.contains(&character)
    })
}

/// Whether the piece ending at a full stop ends in an abbreviation rather than
/// in a sentence.
fn is_abbreviation(piece: &str) -> bool {
    let word = piece.trim_end_matches(TERMINATORS);
    let word = &word[word
        .rfind(|character: char| !(character.is_alphanumeric() || character == '.'))
        .map_or(0, |offset| offset + 1)..];

    // `Returns A. B is the other one.` - a lone capital is an initial, not a
    // sentence.
    if word.chars().count() == 1 && word.chars().all(char::is_uppercase) {
        return true;
    }
    let word = word.to_ascii_lowercase();
    ABBREVIATIONS.contains(&word.as_str())
}

/// Puts translated sentences back together as the one line they came from.
///
/// Japanese carries its own sentence break in `。`, so nothing is inserted
/// after one. A translation that ends in something else - a fragment, a
/// heading, a unit the engine returned unchanged - gets a space, because
/// running two of those together would read as one word.
pub fn join_sentences(translations: &[String]) -> String {
    let mut joined = String::new();
    for translation in translations {
        let translation = translation.trim();
        if translation.is_empty() {
            continue;
        }
        if !joined.is_empty() && !joined.ends_with(['。', '！', '？', '．', '.', '!', '?']) {
            joined.push(' ');
        }
        joined.push_str(translation);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_sentence_is_one_piece() {
        assert_eq!(
            split_sentences("Returns the currently authenticated user."),
            ["Returns the currently authenticated user."]
        );
        assert_eq!(
            split_sentences("No full stop at all"),
            ["No full stop at all"]
        );
    }

    #[test]
    fn nothing_comes_out_of_nothing() {
        assert!(split_sentences("").is_empty());
        assert!(split_sentences("   ").is_empty());
    }

    /// The failure this module exists for.
    #[test]
    fn a_paragraph_is_cut_at_its_boundaries() {
        assert_eq!(
            split_sentences(
                "Returns the user. Fails when the id is unknown. The cache is not consulted."
            ),
            [
                "Returns the user.",
                "Fails when the id is unknown.",
                "The cache is not consulted.",
            ]
        );
    }

    #[test]
    fn a_terminator_that_is_not_a_boundary_does_not_cut() {
        for text in [
            // No white space after it.
            "v0.1: cache, then translate.",
            "Calls a.b and returns.",
            // Nothing that opens a sentence after it.
            "Ends in a full stop. then carries on lower case.",
            // An abbreviation.
            "Uses a cache, e.g. an LRU one.",
            "Wraps the reader, the writer, etc. Nothing else.  ",
            // A masked span is one token, dots and all.
            "See X0Q for the protocol.",
        ] {
            let pieces = split_sentences(text);
            assert_eq!(pieces[0], text.trim(), "cut {text:?} into {pieces:?}");
        }
    }

    #[test]
    fn an_abbreviation_mid_paragraph_keeps_its_sentence_whole() {
        assert_eq!(
            split_sentences("Uses a cache, e.g. an LRU one. Nothing else is stored."),
            ["Uses a cache, e.g. an LRU one.", "Nothing else is stored."]
        );
    }

    #[test]
    fn a_run_of_terminators_is_one_boundary() {
        assert_eq!(
            split_sentences("Wait... The rest arrives later."),
            ["Wait...", "The rest arrives later."]
        );
        assert_eq!(
            split_sentences("Really?! It does."),
            ["Really?!", "It does."]
        );
    }

    #[test]
    fn a_sentence_can_open_with_a_placeholder_or_a_quote() {
        assert_eq!(
            split_sentences("Nothing else. X0Q is the default."),
            ["Nothing else.", "X0Q is the default."]
        );
        assert_eq!(
            split_sentences("Nothing else. `None` is the default."),
            ["Nothing else.", "`None` is the default."]
        );
    }

    #[test]
    fn splitting_loses_no_prose() {
        let text = "Returns the user. Fails when the id is unknown. Nothing is cached.";
        assert_eq!(split_sentences(text).join(" "), text);
    }

    #[test]
    fn japanese_sentences_are_joined_without_a_space() {
        assert_eq!(
            join_sentences(&[
                "ユーザを返します。".to_owned(),
                "IDが不明な時に失敗します。".to_owned(),
            ]),
            "ユーザを返します。IDが不明な時に失敗します。"
        );
    }

    #[test]
    fn a_translation_without_a_sentence_break_gets_a_space() {
        assert_eq!(
            join_sentences(&["スレッドセーフ".to_owned(), "べき等です".to_owned()]),
            "スレッドセーフ べき等です"
        );
    }

    #[test]
    fn joining_skips_what_the_engine_left_empty() {
        assert_eq!(
            join_sentences(&[String::new(), "本文。".to_owned(), "  ".to_owned()]),
            "本文。"
        );
    }
}
