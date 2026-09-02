//! Cutting a paragraph into the sentences the engine is meant to see.
//!
//! FuguMT is a sentence-level model, and [`CommentShape`] hands it paragraphs:
//! a run of `//` lines is merged into one unit so that a sentence spread over
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

/// Characters that can end a clause without ending a sentence.
///
/// A long sentence hinged on one of these is truncated exactly like a
/// paragraph is - the engine translates one side and stops:
///
/// ```text
/// The deduplication is a second one, below the one X0Q does: two comments that
/// differ only in their indentation share a single segment.
///   whole -> インデントのみが異なる2つのコメント…       (everything before the colon is gone)
///   split -> 重複排除は2番目のもので、X0Qが行うものです。
///            インデントのみが異なる2つのコメントは…
/// ```
const CLAUSE_TERMINATORS: [char; 2] = [';', ':'];

/// How many words each side of a clause break needs before it is one.
///
/// `IMPORTANT: what is hashed ...` and `v0.1: cache, then translate` hinge on a
/// colon that introduces the sentence rather than dividing it, and the give-away
/// is that there is nearly nothing on the left. The same count guards the right,
/// so a trailing `see:` does not become a unit of its own.
const MIN_CLAUSE_WORDS: usize = 4;

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
        if !TERMINATORS.contains(&character) && !CLAUSE_TERMINATORS.contains(&character) {
            cursor += character.len_utf8();
            continue;
        }

        // `...` and `?!` end one sentence between them, not three.
        let mut end = cursor;
        while text[end..].starts_with(TERMINATORS) {
            end += 1;
        }
        if end == cursor {
            end += character.len_utf8();
        }

        let rest = &text[end..];
        let next = rest.trim_start();
        // Every boundary needs white space after it: `v0.1` and `a.b` are one
        // token.
        if next.len() == rest.len() || !is_a_boundary(&text[start..end], next) {
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

/// Whether the terminator between `before` and `after` divides two units.
///
/// A sentence end and a clause end are told apart by different things. After a
/// full stop the next word is capitalised, which is nearly proof on its own;
/// after a semicolon or a colon it is not, so the length of the two sides is
/// all there is to go on.
fn is_a_boundary(before: &str, after: &str) -> bool {
    if before.ends_with(CLAUSE_TERMINATORS) {
        return words(before) >= MIN_CLAUSE_WORDS && words(after) >= MIN_CLAUSE_WORDS;
    }
    opens_a_sentence(after) && !is_abbreviation(before)
}

fn words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
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
    // The last word starts after the separator before it. `char_indices`
    // rather than `rfind`, because `rfind` gives the offset the separator
    // *starts* at: adding one lands inside it whenever it is not one byte, and
    // slicing there panics. A comment carrying `§7.2.` or a pasted `”` is
    // enough, and this runs on the worker's task rather than inside
    // `spawn_blocking`, so the panic took translation down for the session.
    let start = word
        .char_indices()
        .rev()
        .find(|(_, character)| !(character.is_alphanumeric() || *character == '.'))
        .map_or(0, |(offset, character)| offset + character.len_utf8());
    let word = &word[start..];

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

    /// `rfind` reports where a character starts, so the old `+ 1` sliced into
    /// any separator wider than a byte and panicked. These are the two shapes
    /// that reach it from real comments: a section sign written before a
    /// numbered reference, and a quotation mark pasted in from prose.
    #[test]
    fn a_multibyte_separator_before_a_full_stop_does_not_panic() {
        assert_eq!(
            split_sentences("See docs \u{a7}7.2. The rest is here."),
            ["See docs \u{a7}7.2.", "The rest is here."]
        );
        assert_eq!(
            split_sentences("It is called \u{201c}the handle\u{201d}. The rest is here."),
            [
                "It is called \u{201c}the handle\u{201d}.",
                "The rest is here."
            ]
        );
    }

    /// The separator is found the same way it was; only the offset changed.
    #[test]
    fn the_last_word_is_still_what_decides_an_abbreviation() {
        assert!(is_abbreviation("e.g."));
        assert!(is_abbreviation("Returns A."));
        assert!(!is_abbreviation("the socket."));
        // No separator at all: the whole piece is the word.
        assert!(is_abbreviation("etc."));
    }

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

    /// A long sentence hinged on a semicolon or a colon is two units.
    #[test]
    fn a_clause_break_with_enough_on_both_sides_is_a_boundary() {
        assert_eq!(
            split_sentences(
                "Translation is serialised so that one inference runs at a time; X0Q's pool \
                 would otherwise start hundreds."
            ),
            [
                "Translation is serialised so that one inference runs at a time;",
                "X0Q's pool would otherwise start hundreds.",
            ]
        );
        assert_eq!(
            split_sentences("The shutdown is not graceful: requests in flight are abandoned."),
            [
                "The shutdown is not graceful:",
                "requests in flight are abandoned.",
            ]
        );
    }

    /// A colon that introduces a sentence rather than dividing it has nearly
    /// nothing on its left, and a trailing one has nothing on its right.
    #[test]
    fn a_colon_with_too_little_on_one_side_is_not_a_boundary() {
        for text in [
            "X0Q what is hashed is the comment as the file has it.",
            "v0.1: cache, then translate, then refresh.",
            "Shape of one round trip:",
            "Uses a map; nothing else.",
        ] {
            let pieces = split_sentences(text);
            assert_eq!(pieces[0], text.trim(), "cut {text:?} into {pieces:?}");
        }
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
