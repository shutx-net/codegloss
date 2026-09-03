//! The file format the `extract` example writes and the measurement harnesses
//! read: the comments of some source files, one after another, under a line
//! naming the rules they were read with.
//!
//! ```text
//! %%% rules: indented
//! // Package fs defines basic interfaces to a file system.
//! %%%
//! // ReadFile reads the named file and returns its contents.
//! ```
//!
//! Reading is here, in the crate whose example does the writing, because the
//! two drifting apart is the whole of Issue #62: `extract` learned `--lang` in
//! #59 and kept writing a file that could not say so, and every reader went on
//! assuming [`CommentRules::Fenced`]. A Go corpus scored that way hands the
//! engine indented examples as prose and the scoreboard of
//! `docs/model-runtime-notes.md` §12 measures something that does not happen.
//!
//! **A file with no header reads as [`CommentRules::Fenced`].** Every corpus
//! written before the header existed is Rust - `codegloss-parser` read nothing
//! else until #59 - so the fallback is not a guess, and the frozen corpus of
//! `codegloss-translator/tests/fixtures/comment-corpus.txt` keeps reading
//! correctly without being rewritten.
//!
//! The rules are named once, at the top, rather than beside each `%%%`. One
//! `extract` run has one `--lang`, so a file has one set of rules by
//! construction and a per-block marker would encode a freedom the writer does
//! not have; the readers match the separator as the exact line `%%%`, which a
//! field hung off it would break; and the first block has no separator in front
//! of it, so a per-block scheme needs a header for it anyway.

use codegloss_core::CommentRules;

/// What a header line says before the tag.
///
/// `%%%` rather than a new token: it is the one sequence the format already
/// reserves, and no comment of any language CodeGloss reads begins with it.
const HEADER: &str = "%%% rules: ";

/// The header naming `rules`, newline included - what `extract` puts at the top
/// of a corpus.
pub fn header(rules: CommentRules) -> String {
    format!("{HEADER}{}\n", rules.tag())
}

/// A header line naming rules this build does not have.
///
/// An error rather than a fallback to [`CommentRules::Fenced`]: a file that
/// names its rules and is read under different ones is exactly the silent
/// mis-scoring the header exists to stop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("corpus header names rules this build does not have: {0:?}")]
pub struct UnknownRules(pub String);

/// Splits a corpus into the rules it declares and the blocks under them.
///
/// Returns the text unchanged beside [`CommentRules::Fenced`] when there is no
/// header, so a caller can go on splitting on `%%%` exactly as before.
pub fn rules(text: &str) -> Result<(CommentRules, &str), UnknownRules> {
    let Some(rest) = text.strip_prefix(HEADER) else {
        return Ok((CommentRules::Fenced, text));
    };
    let (tag, blocks) = rest.split_once('\n').unwrap_or((rest, ""));
    // Trimmed before it is matched: a corpus is a file people hand-edit and
    // move between machines, so a trailing `\r` from a CRLF checkout is not a
    // different set. `CommentRules::from_tag` itself stays exact - it also
    // reads cache keys, where a stray byte is not a typo.
    match CommentRules::from_tag(tag.trim()) {
        Some(rules) => Ok((rules, blocks)),
        None => Err(UnknownRules(tag.trim().to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corpus_without_a_header_is_fenced_and_untouched() {
        let text = "/// One.\n%%%\n/// Two.\n";
        assert_eq!(rules(text), Ok((CommentRules::Fenced, text)));
    }

    #[test]
    fn the_header_names_the_rules_and_is_taken_off() {
        assert_eq!(
            rules("%%% rules: indented\n// One.\n"),
            Ok((CommentRules::Indented, "// One.\n"))
        );
        assert_eq!(
            rules("%%% rules: fenced\n/// One.\n"),
            Ok((CommentRules::Fenced, "/// One.\n"))
        );
    }

    #[test]
    fn what_extract_writes_is_what_this_reads() {
        for written in [CommentRules::Fenced, CommentRules::Indented] {
            let text = format!("{}// One.\n", header(written));
            assert_eq!(rules(&text), Ok((written, "// One.\n")));
        }
    }

    /// A corpus written by a newer build fails here rather than being scored
    /// under whatever this one happens to default to.
    #[test]
    fn an_unknown_tag_is_an_error_rather_than_a_panic() {
        assert_eq!(
            rules("%%% rules: python\n// One.\n"),
            Err(UnknownRules("python".to_owned()))
        );
        assert_eq!(
            rules("%%% rules: \n// One.\n"),
            Err(UnknownRules(String::new()))
        );
    }

    /// Truncated, unterminated and empty input all answer instead of panicking:
    /// this reads files, and `probe` turns the answer into an exit code.
    #[test]
    fn a_malformed_corpus_answers_rather_than_panicking() {
        assert_eq!(rules(""), Ok((CommentRules::Fenced, "")));
        assert_eq!(rules("%%%"), Ok((CommentRules::Fenced, "%%%")));
        assert_eq!(
            rules("%%% rules:"),
            Ok((CommentRules::Fenced, "%%% rules:"))
        );
        // A header with no newline after it: the tag runs to the end and there
        // are no blocks.
        assert_eq!(
            rules("%%% rules: indented"),
            Ok((CommentRules::Indented, ""))
        );
        assert_eq!(
            rules("%%% rules: nope"),
            Err(UnknownRules("nope".to_owned()))
        );
    }

    /// A CRLF checkout names the same rules a LF one does.
    #[test]
    fn a_carriage_return_is_not_a_different_set() {
        assert_eq!(
            rules("%%% rules: indented\r\n// One.\r\n"),
            Ok((CommentRules::Indented, "// One.\r\n"))
        );
    }
}
