//! Domain types.
//!
//! Positions are expressed as zero-based line numbers plus byte offsets into
//! the source file. LSP `Position` / `Range` are intentionally not used here so
//! that this crate stays editor-agnostic.

use serde::{Deserialize, Serialize};

use crate::CommentRules;

/// How a comment was written in the source file.
///
/// Stamped by the parser off the syntax tree, and read today only by the
/// extraction tests that check it stamped the right thing. **Post-processing
/// does not use it**: `docblock` reads the structure of a comment back off
/// [`CommentBlock::raw`] instead, because the shape it has to rebuild - which
/// lines are a paragraph, a tag line, a fence - is finer than these four names
/// and is in the text either way. Kept because it is what those tests assert
/// against, which is worth more than the field costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommentStyle {
    /// `// ...`
    Line,
    /// `/* ... */`
    Block,
    /// `/// ...` or `//! ...`
    DocLine,
    /// `/** ... */`
    DocBlock,
}

/// One contiguous run of comments, treated as a single unit of translation.
///
/// Consecutive line comments are merged into one block so that a sentence split
/// across several `//` lines is translated as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentBlock {
    /// See [`CommentStyle`]: written here by the parser, read by its tests.
    pub style: CommentStyle,
    /// The shape rules of the language this comment was read from.
    ///
    /// Stamped by the parser, which is the one place that knows the language,
    /// and carried from here into [`CommentShape::parse`](crate::CommentShape)
    /// and the cache key. The rules decide how the block comes apart, so they
    /// travel with it rather than being assumed further down.
    pub rules: CommentRules,
    /// The prose of the comment: markers (`//`, `///`, `/*`, `*/`, the leading
    /// `*` of a continuation line) stripped, lines joined with a single space.
    ///
    /// This is what gets pre-processed, hashed into a [`GlossKey`] and handed
    /// to the translator, so it must not carry syntax the model would try to
    /// translate.
    pub text: String,
    /// The block exactly as it appears in the file, markers and interior
    /// indentation included.
    ///
    /// Post-processing needs it: rebuilding the shape of a Javadoc block, or
    /// re-indenting a translated run of `//` lines, is impossible once the
    /// markers and the line breaks are gone. Keeping it beside `text` is what
    /// lets `text` stay clean.
    pub raw: String,
    /// Zero-based line of the first line of the block.
    pub start_line: u32,
    /// Zero-based line of the last line of the block, inclusive.
    pub end_line: u32,
    /// Byte offset of the first byte of the block.
    pub start_byte: usize,
    /// Byte offset one past the last byte of the block.
    pub end_byte: usize,
}

/// One unit of translation: the plain text of a single request to the engine.
///
/// A distinct type rather than a bare `String` on purpose. Pre-processing has
/// to hand the engine a text whose identifiers, back-quoted code and URLs have
/// been swapped for placeholders, plus the table that puts them back; that
/// table will live here, and every call site already speaks in `Segment`s by
/// the time it does.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Segment {
    text: String,
}

impl Segment {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The text to send to the engine.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

impl From<String> for Segment {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Segment {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Cache key for a translation: `BLAKE3(model_version, src_lang, tgt_lang, text)`.
///
/// The four inputs are hashed with a NUL separator between them so that no two
/// different tuples can produce the same byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GlossKey(pub [u8; 32]);

/// Version of the pre- and post-processing a gloss was produced with.
///
/// Hashed into every [`GlossKey`] beside the model version. What is cached is
/// the finished gloss, so a change to how a comment is cut into units, masked
/// or rebuilt changes the answer without the engine changing at all - and a
/// cache directory outlives a release. Without this, an upgrade would keep
/// serving what the old code wrote, for as long as the entry survives.
///
/// **Bump it whenever `preserve`, `sentence` or `docblock` changes what comes
/// out of the pipeline.** The stability test in this module fails when the key
/// encoding moves, so a bump is a deliberate act rather than a side effect.
///
/// - `1` - one segment per unit.
/// - `2` - one segment per sentence.
/// - `3` - a Javadoc inline tag is one protected span.
/// - `4` - a relative clause after a comma is its own sentence.
/// - `5` - an English sentence end no longer swallows the space after it.
/// - `6` - the indentation inside a fence is kept, and a tilde rule is not a
///   fence.
/// - `7` - a block carries the shape rules of its language, and they are part
///   of the key.
pub const PIPELINE_VERSION: &str = "7";

impl GlossKey {
    /// Separator between the hashed fields. NUL cannot appear in a language tag
    /// or a model version, which is what keeps the encoding unambiguous.
    const SEPARATOR: &'static [u8] = b"\0";

    pub fn new(
        rules: CommentRules,
        model_version: &str,
        src_lang: &str,
        tgt_lang: &str,
        text: &str,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        for field in [
            PIPELINE_VERSION,
            rules.tag(),
            model_version,
            src_lang,
            tgt_lang,
            text,
        ] {
            hasher.update(field.as_bytes());
            hasher.update(Self::SEPARATOR);
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Lowercase hex form, for logs and for on-disk cache file names.
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key encoding is a storage format: entries written by an earlier
    /// build have to keep hashing to the same name, and a change that moves
    /// every key has to be a bump of [`PIPELINE_VERSION`] rather than an
    /// accident. Both directions of that are this assertion.
    #[test]
    fn the_key_encoding_is_stable() {
        assert_eq!(
            GlossKey::new(
                CommentRules::Fenced,
                "fugumt-en-ja@1",
                "en",
                "ja",
                "Returns the user."
            )
            .to_hex(),
            "ae2414b77fa4f402d49b848c47c452b28c69e05894a9ea5bc02fa8934804ce22"
        );
    }

    #[test]
    fn a_segment_carries_its_text_unchanged() {
        let segment = Segment::new("Return the cached user.");
        assert_eq!(segment.text(), "Return the cached user.");
        assert_eq!(
            Segment::from("x".to_owned()).into_text(),
            Segment::from("x").into_text()
        );
    }

    #[test]
    fn same_inputs_produce_the_same_key() {
        let a = GlossKey::new(
            CommentRules::Fenced,
            "fugumt-en-ja@1",
            "en",
            "ja",
            "Return the cached user.",
        );
        let b = GlossKey::new(
            CommentRules::Fenced,
            "fugumt-en-ja@1",
            "en",
            "ja",
            "Return the cached user.",
        );
        assert_eq!(a, b);
    }

    /// The same sentence read under two sets of rules is two glosses, because
    /// the rules decide what of it is prose at all. `// Returns the user.` is
    /// a comment in every language CodeGloss reads, so nothing else in the key
    /// would tell those two apart.
    #[test]
    fn the_rules_are_part_of_every_key() {
        assert_ne!(
            GlossKey::new(
                CommentRules::Fenced,
                "m",
                "en",
                "ja",
                "// Returns the user."
            ),
            GlossKey::new(
                CommentRules::Indented,
                "m",
                "en",
                "ja",
                "// Returns the user."
            )
        );
    }

    #[test]
    fn every_field_changes_the_key() {
        let base = GlossKey::new(CommentRules::Fenced, "m", "en", "ja", "text");
        assert_ne!(
            base,
            GlossKey::new(CommentRules::Indented, "m", "en", "ja", "text")
        );
        assert_ne!(
            base,
            GlossKey::new(CommentRules::Fenced, "m2", "en", "ja", "text")
        );
        assert_ne!(
            base,
            GlossKey::new(CommentRules::Fenced, "m", "de", "ja", "text")
        );
        assert_ne!(
            base,
            GlossKey::new(CommentRules::Fenced, "m", "en", "fr", "text")
        );
        assert_ne!(
            base,
            GlossKey::new(CommentRules::Fenced, "m", "en", "ja", "other")
        );
    }

    #[test]
    fn field_boundaries_are_not_ambiguous() {
        // Without a separator these two would hash the same byte stream.
        assert_ne!(
            GlossKey::new(CommentRules::Fenced, "ab", "c", "ja", "text"),
            GlossKey::new(CommentRules::Fenced, "a", "bc", "ja", "text")
        );
    }

    #[test]
    fn hex_is_64_lowercase_digits() {
        let hex = GlossKey::new(CommentRules::Fenced, "m", "en", "ja", "text").to_hex();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }
}
