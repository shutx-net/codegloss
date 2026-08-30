//! Protecting the spans of a comment that must survive translation unchanged.
//!
//! An NMT model translates everything it is given, identifiers included:
//! `Returns \`UserDetails\` when authentication succeeds.` comes back with
//! `UserDetails` translated, transliterated or dropped. The fix is to hide
//! those spans behind placeholders before the engine sees them and to put them
//! back afterwards, which is what [`mask`] and [`Masked::unmask`] do.
//!
//! IMPORTANT (AGENTS.md): this lives here rather than inside a [`Translator`]
//! implementation. An engine that hid this inside itself would take it along
//! the day it is replaced.
//!
//! [`Translator`]: https://docs.rs/codegloss-translator

use serde::{Deserialize, Serialize};

/// Opening delimiter of a placeholder.
///
/// PROVISIONAL. Which form survives FuguMT's SentencePiece tokenizer without
/// being split, dropped or reordered is measured in P7; the candidates are this
/// pair of non-ASCII brackets and an underscore form such as `__CG0__`. Nothing
/// outside this module knows what a placeholder looks like: [`placeholder`]
/// writes them and [`placeholder_at`] reads them, and swapping the format means
/// editing those two functions and nothing else.
///
/// The brackets are the provisional choice because they cannot collide with
/// English prose, which is what makes masking safe in the first place. Their
/// weakness is the other half of the question: a token outside the vocabulary
/// may come back as `<unk>`. That is exactly what P7 has to measure.
const PLACEHOLDER_OPEN: char = '⟦';

/// Closing delimiter. See [`PLACEHOLDER_OPEN`].
const PLACEHOLDER_CLOSE: char = '⟧';

/// The placeholder that stands in for the `index`-th protected span.
pub fn placeholder(index: usize) -> String {
    format!("{PLACEHOLDER_OPEN}{index}{PLACEHOLDER_CLOSE}")
}

/// Inverse of [`placeholder`]: the index it encodes and the bytes it occupies,
/// when `text` starts with one.
fn placeholder_at(text: &str) -> Option<(usize, usize)> {
    let body = text.strip_prefix(PLACEHOLDER_OPEN)?;
    let end = body.find(PLACEHOLDER_CLOSE)?;
    let digits = &body[..end];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    Some((
        digits.parse().ok()?,
        PLACEHOLDER_OPEN.len_utf8() + digits.len() + PLACEHOLDER_CLOSE.len_utf8(),
    ))
}

/// One span that was taken out of the text before translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preserved {
    text: String,
}

impl Preserved {
    /// The original span, exactly as it appeared in the comment.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A comment with its protected spans replaced by placeholders, together with
/// the table that puts them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Masked {
    source: String,
    masked: String,
    preserved: Vec<Preserved>,
}

impl Masked {
    /// The text as it was written.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The text to hand to the engine.
    pub fn masked(&self) -> &str {
        &self.masked
    }

    /// The protected spans, in the order their placeholders were allocated.
    pub fn preserved(&self) -> &[Preserved] {
        &self.preserved
    }

    /// Puts the protected spans back into a translation of [`Self::masked`].
    ///
    /// Placeholders are matched by the index they carry, not by position, so a
    /// translation that moved them around - Japanese word order routinely does -
    /// restores correctly.
    ///
    /// A translation that lost one of them cannot be repaired: the span it
    /// stood for has nowhere to go, and emitting the rest would silently drop an
    /// identifier. Such a translation is discarded and the English source is
    /// returned instead, which is wrong in an obvious way rather than in a
    /// subtle one.
    pub fn unmask(&self, translated: &str) -> String {
        if self.preserved.is_empty() {
            return translated.to_owned();
        }

        let mut restored = String::with_capacity(translated.len());
        let mut seen = vec![false; self.preserved.len()];
        let mut rest = translated;

        // One pass, never a repeated `replace`: a restored span may itself look
        // like a placeholder - a comment quoting one - and a second pass would
        // substitute into what the first pass just wrote.
        while let Some(offset) = rest.find(PLACEHOLDER_OPEN) {
            restored.push_str(&rest[..offset]);
            rest = &rest[offset..];

            match placeholder_at(rest) {
                Some((index, length)) if index < self.preserved.len() => {
                    restored.push_str(self.preserved[index].text());
                    seen[index] = true;
                    rest = &rest[length..];
                }
                // An index nobody allocated, or a stray bracket: copied through
                // as the text it is.
                _ => {
                    restored.push(PLACEHOLDER_OPEN);
                    rest = &rest[PLACEHOLDER_OPEN.len_utf8()..];
                }
            }
        }
        restored.push_str(rest);

        if seen.iter().all(|found| *found) {
            restored
        } else {
            self.source.clone()
        }
    }
}

/// Replaces every span that must not be translated with a placeholder.
///
/// Spans never overlap and are found left to right, so the first rule that
/// matches at a position wins. In priority order:
///
/// 1. a placeholder the comment already contained, protected so that it cannot
///    be confused with one of ours
/// 2. inline code between back quotes
/// 3. an `http://` or `https://` URL
/// 4. a doc tag: `@return`, `@param`, `@throws`, ...
/// 5. a `TODO:` / `FIXME:` style prefix at the start of a line
/// 6. a word that reads as code rather than as prose - see [`looks_like_code`]
pub fn mask(text: &str) -> Masked {
    let mut masked = String::with_capacity(text.len());
    let mut preserved: Vec<Preserved> = Vec::new();
    let mut cursor = 0;
    let mut previous: Option<char> = None;
    let mut at_line_start = true;

    while cursor < text.len() {
        if let Some(end) = protected_span(&text[cursor..], previous, at_line_start) {
            let span = &text[cursor..cursor + end];
            masked.push_str(&placeholder(preserved.len()));
            preserved.push(Preserved {
                text: span.to_owned(),
            });
            previous = span.chars().next_back();
            at_line_start = false;
            cursor += end;
            continue;
        }

        let character = text[cursor..]
            .chars()
            .next()
            .expect("the cursor is on a character boundary");
        masked.push(character);
        at_line_start = character == '\n' || (at_line_start && character.is_whitespace());
        previous = Some(character);
        cursor += character.len_utf8();
    }

    Masked {
        source: text.to_owned(),
        masked,
        preserved,
    }
}

/// Length of the protected span starting at the front of `rest`, if there is
/// one. `previous` is the character before it and `at_line_start` says whether
/// only whitespace precedes it on its line.
fn protected_span(rest: &str, previous: Option<char>, at_line_start: bool) -> Option<usize> {
    if let Some((_, length)) = placeholder_at(rest) {
        return Some(length);
    }
    if let Some(length) = inline_code(rest) {
        return Some(length);
    }
    if let Some(length) = url(rest) {
        return Some(length);
    }

    // The rules below match words, and a word only starts where the previous
    // character is not part of one. Without this the tail of `snake_case` would
    // be offered as a word of its own.
    if previous.is_some_and(is_word_character) {
        return None;
    }
    if at_line_start && let Some(length) = attention_prefix(rest) {
        return Some(length);
    }
    if let Some(length) = doc_tag(rest) {
        return Some(length);
    }
    identifier(rest)
}

/// `` `UserDetails` ``, back quotes included.
///
/// A lone back quote is not a match: an apostrophe-like stray one is prose, and
/// swallowing the rest of the comment behind it would be worse than leaving it.
fn inline_code(rest: &str) -> Option<usize> {
    let body = rest.strip_prefix('`')?;
    let end = body.find('`')?;
    if body[..end].contains('\n') {
        return None;
    }
    Some('`'.len_utf8() * 2 + end)
}

/// Schemes recognised as the start of a URL.
///
/// Deliberately short: `foo:` matching a scheme would swallow the rest of any
/// sentence containing a colon.
const URL_SCHEMES: [&str; 2] = ["https://", "http://"];

/// Characters that end a URL, on top of whitespace.
const URL_TERMINATORS: [char; 5] = ['`', '<', '>', '"', '|'];

/// Trailing characters that belong to the sentence rather than to the URL.
const URL_TRAILERS: [char; 8] = ['.', ',', ';', ':', '!', '?', ')', ']'];

fn url(rest: &str) -> Option<usize> {
    let scheme = URL_SCHEMES
        .iter()
        .find(|scheme| rest.get(..scheme.len()) == Some(**scheme))?;

    let mut end = rest
        .char_indices()
        .find(|(_, character)| character.is_whitespace() || URL_TERMINATORS.contains(character))
        .map_or(rest.len(), |(offset, _)| offset);

    // `See https://example.com.` ends in a full stop, and the full stop is
    // punctuation. A path that really ends in one is rare enough to lose.
    while let Some(last) = rest[..end].chars().next_back() {
        if !URL_TRAILERS.contains(&last) {
            break;
        }
        end -= last.len_utf8();
    }

    (end > scheme.len()).then_some(end)
}

/// `@return`, `@param`, `@throws`, `@see`, ... - the tag alone, not its
/// argument.
fn doc_tag(rest: &str) -> Option<usize> {
    let body = rest.strip_prefix('@')?;
    let letters = body.bytes().take_while(u8::is_ascii_alphabetic).count();
    (letters > 0).then_some('@'.len_utf8() + letters)
}

/// Prefixes that mark a comment as an aside rather than as documentation.
///
/// They are conventions a reader greps for, so they have to come back out of
/// the translation spelled exactly as they went in.
const ATTENTION_MARKERS: [&str; 8] = [
    "TODO", "FIXME", "NOTE", "HACK", "XXX", "SAFETY", "WARNING", "Panics",
];

/// `TODO:`, `FIXME(alice):`, `SAFETY:` - marker, optional owner, colon.
///
/// The colon is required: `NOTE` in the middle of a sentence is a word, and
/// `NOTEBOOK:` is not a marker at all.
fn attention_prefix(rest: &str) -> Option<usize> {
    let marker = ATTENTION_MARKERS
        .iter()
        .find(|marker| rest.starts_with(**marker))?;

    let mut end = marker.len();
    if rest[end..].starts_with('(') {
        end += rest[end..].find(')')? + ')'.len_utf8();
    }
    rest[end..].starts_with(':').then_some(end + ':'.len_utf8())
}

fn is_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// A word that reads as code: `find_user`, `UserDetails`, `foo()`, `a::b`.
fn identifier(rest: &str) -> Option<usize> {
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }

    let mut end = 0;
    loop {
        end += rest[end..]
            .chars()
            .take_while(|character| is_word_character(*character))
            .map(char::len_utf8)
            .sum::<usize>();

        let tail = &rest[end..];
        // A path or a field access continues the same token, but only when a
        // word follows: the full stop of `Returns the user.` does not.
        if tail.starts_with("::") && tail[2..].starts_with(is_word_start) {
            end += 2;
            continue;
        }
        if tail.starts_with('.') && tail[1..].starts_with(is_word_start) {
            end += 1;
            continue;
        }
        if tail.starts_with("()") {
            end += 2;
        }
        break;
    }

    looks_like_code(&rest[..end]).then_some(end)
}

fn is_word_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

/// Whether a word is code rather than an English word.
///
/// IMPORTANT: this test stays conservative on purpose. A rule that also caught
/// single capitalised words would protect `Returns`, `Fails`, `Panics` and every
/// other sentence opener, and the comment would come back untranslated. Only a
/// word carrying a mark prose does not use qualifies: an underscore, a path
/// separator, a call's parentheses, or a case boundary inside the word.
fn looks_like_code(word: &str) -> bool {
    word.contains('_') || word.contains("::") || word.ends_with("()") || has_case_boundary(word)
}

/// `userId`, `UserDetails`, `HTTPServer` - but not `Returns` or `USA`.
fn has_case_boundary(word: &str) -> bool {
    let characters: Vec<char> = word.chars().collect();
    characters
        .windows(2)
        .any(|pair| pair[0].is_lowercase() && pair[1].is_uppercase())
        || characters.windows(3).any(|triple| {
            triple[0].is_uppercase() && triple[1].is_uppercase() && triple[2].is_lowercase()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Masking and restoring an untranslated text has to be the identity. Every
    /// rule below is checked through this, because a rule that protects a span
    /// it cannot put back is worse than no rule at all.
    fn round_trip(text: &str) -> String {
        let masked = mask(text);
        masked.unmask(masked.masked())
    }

    fn spans(text: &str) -> Vec<String> {
        mask(text)
            .preserved
            .into_iter()
            .map(|preserved| preserved.text)
            .collect()
    }

    #[test]
    fn a_text_without_anything_to_protect_is_left_alone() {
        let masked = mask("Return the cached user.");
        assert_eq!(masked.masked(), "Return the cached user.");
        assert!(masked.preserved().is_empty());
        assert_eq!(
            masked.unmask("キャッシュされたユーザーを返す。"),
            "キャッシュされたユーザーを返す。"
        );
    }

    #[test]
    fn the_placeholder_format_reads_back_as_what_it_wrote() {
        for index in [0, 1, 9, 10, 4096] {
            let written = placeholder(index);
            assert_eq!(placeholder_at(&written), Some((index, written.len())));
        }
        assert_eq!(placeholder_at("not a placeholder"), None);
        assert_eq!(placeholder_at("⟦⟧"), None);
        assert_eq!(placeholder_at("⟦x⟧"), None);
    }

    /// The example Issue #1 singles out.
    #[test]
    fn inline_code_is_protected_with_its_back_quotes() {
        let masked = mask("Returns `UserDetails` when authentication succeeds.");
        assert_eq!(masked.masked(), "Returns ⟦0⟧ when authentication succeeds.");
        assert_eq!(masked.preserved()[0].text(), "`UserDetails`");
        assert_eq!(
            masked.unmask("認証に成功すると ⟦0⟧ を返します。"),
            "認証に成功すると `UserDetails` を返します。"
        );
    }

    #[test]
    fn an_unclosed_back_quote_is_prose() {
        assert!(spans("It isn't `finished").is_empty());
    }

    #[test]
    fn a_url_is_protected_without_the_punctuation_that_follows_it() {
        assert_eq!(
            spans("See https://example.com/a_b for details."),
            ["https://example.com/a_b".to_owned()]
        );
        assert_eq!(
            spans("See https://example.com."),
            ["https://example.com".to_owned()]
        );
        assert_eq!(
            spans("(https://example.com/x)"),
            ["https://example.com/x".to_owned()]
        );
        assert_eq!(spans("Not a url: https://"), Vec::<String>::new());
    }

    #[test]
    fn doc_tags_are_protected_but_their_arguments_are_not() {
        let masked = mask("@return authenticated user");
        assert_eq!(masked.masked(), "⟦0⟧ authenticated user");
        assert_eq!(masked.preserved()[0].text(), "@return");
    }

    #[test]
    fn attention_prefixes_are_protected_at_the_start_of_a_line() {
        assert_eq!(spans("TODO: drop this."), ["TODO:".to_owned()]);
        assert_eq!(
            spans("FIXME(alice): drop this."),
            ["FIXME(alice):".to_owned()]
        );
        assert_eq!(
            spans("SAFETY: the pointer is aligned."),
            ["SAFETY:".to_owned()]
        );
        // Mid-sentence, and without a colon, they are ordinary words.
        assert!(spans("Please note: nothing.").is_empty());
        assert!(spans("NOTEBOOK: not a marker.").is_empty());
    }

    #[test]
    fn identifiers_are_protected_and_english_words_are_not() {
        for (text, expected) in [
            ("Calls find_user first.", vec!["find_user"]),
            ("Returns UserDetails here.", vec!["UserDetails"]),
            ("Calls fetch() twice.", vec!["fetch()"]),
            ("See codegloss::core for it.", vec!["codegloss::core"]),
            ("Wraps a HTTPServer instance.", vec!["HTTPServer"]),
            ("Returns the currently authenticated user.", vec![]),
            ("Fails when the id is unknown.", vec![]),
            // A sentence full stop is not a field access, and `e.g.` is prose.
            ("Loads the user. Returns none.", vec![]),
            ("Uses a cache, e.g. an LRU one.", vec![]),
        ] {
            assert_eq!(spans(text), expected, "in {text:?}");
        }
    }

    /// Japanese word order moves the placeholders around; matching them by
    /// index rather than by position is what makes that harmless.
    #[test]
    fn a_translation_that_reorders_the_placeholders_still_restores() {
        let masked = mask("Calls `load()` before find_user.");
        assert_eq!(masked.preserved().len(), 2);
        assert_eq!(
            masked.unmask("⟦1⟧ の前に ⟦0⟧ を呼ぶ。"),
            "find_user の前に `load()` を呼ぶ。"
        );
    }

    /// The fallback the pipeline depends on: a lost placeholder means a lost
    /// identifier, and the English is better than a gloss missing one.
    #[test]
    fn a_translation_that_lost_a_placeholder_falls_back_to_the_source() {
        let masked = mask("Returns `UserDetails` on success.");
        assert_eq!(
            masked.unmask("成功時に返します。"),
            "Returns `UserDetails` on success."
        );
    }

    #[test]
    fn a_placeholder_the_engine_invented_is_left_as_text() {
        let masked = mask("Returns `UserDetails`.");
        // The one that was allocated comes back; `⟦7⟧` was never handed out.
        assert_eq!(masked.unmask("⟦0⟧ ⟦7⟧"), "`UserDetails` ⟦7⟧");
    }

    /// A comment that already contains something shaped like a placeholder is
    /// protected as itself, so restoring cannot substitute into it.
    #[test]
    fn a_comment_containing_a_placeholder_survives_it() {
        let text = "The marker ⟦0⟧ is a placeholder.";
        let masked = mask(text);
        assert_eq!(masked.preserved()[0].text(), "⟦0⟧");
        assert_eq!(round_trip(text), text);
    }

    #[test]
    fn every_protected_pattern_round_trips_untranslated() {
        for text in [
            "Returns `UserDetails` when authentication succeeds.",
            "See https://example.com/docs for the protocol.",
            "@throws AuthenticationException if authentication failed",
            "TODO: replace find_user with UserRepository::load().",
            "SAFETY: `ptr` is aligned and non-null.",
            "Fails when the id is unknown.",
            "日本語のコメント。",
            "",
        ] {
            assert_eq!(round_trip(text), text, "did not round-trip: {text:?}");
        }
    }

    /// Two spans of different rules in one sentence keep their own identities.
    #[test]
    fn several_rules_can_fire_in_one_text() {
        let masked = mask("TODO: see https://example.com about `UserDetails` and find_user.");
        assert_eq!(masked.preserved().len(), 4, "{:?}", masked.preserved());
        assert_eq!(masked.unmask(masked.masked()), masked.source());
    }
}
