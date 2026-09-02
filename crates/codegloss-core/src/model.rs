//! Domain types.
//!
//! Positions are expressed as zero-based line numbers plus byte offsets into
//! the source file. LSP `Position` / `Range` are intentionally not used here so
//! that this crate stays editor-agnostic.

use serde::{Deserialize, Serialize};

/// How a comment was written in the source file.
///
/// The distinction matters for post-processing: doc comments carry structure
/// (`@param`, `@return`, Markdown) that has to survive translation, while a
/// plain line comment does not.
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
    pub style: CommentStyle,
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

/// A finished translation of one comment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gloss {
    /// Text that was fed to the translator, after pre-processing.
    pub source: String,
    /// Translated text, after post-processing.
    pub translated: String,
    /// Identifier of the model that produced `translated`. Part of the cache
    /// key, so swapping models invalidates old entries instead of serving them.
    pub model_version: String,
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
pub const PIPELINE_VERSION: &str = "5";

impl GlossKey {
    /// Separator between the hashed fields. NUL cannot appear in a language tag
    /// or a model version, which is what keeps the encoding unambiguous.
    const SEPARATOR: &'static [u8] = b"\0";

    pub fn new(model_version: &str, src_lang: &str, tgt_lang: &str, text: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        for field in [PIPELINE_VERSION, model_version, src_lang, tgt_lang, text] {
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
            GlossKey::new("fugumt-en-ja@1", "en", "ja", "Returns the user.").to_hex(),
            "0190a04bef87bcc9e895441c0ebab1791f2df38a9c91a7498fc30f828c7b149f"
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
        let a = GlossKey::new("fugumt-en-ja@1", "en", "ja", "Return the cached user.");
        let b = GlossKey::new("fugumt-en-ja@1", "en", "ja", "Return the cached user.");
        assert_eq!(a, b);
    }

    #[test]
    fn every_field_changes_the_key() {
        let base = GlossKey::new("m", "en", "ja", "text");
        assert_ne!(base, GlossKey::new("m2", "en", "ja", "text"));
        assert_ne!(base, GlossKey::new("m", "de", "ja", "text"));
        assert_ne!(base, GlossKey::new("m", "en", "fr", "text"));
        assert_ne!(base, GlossKey::new("m", "en", "ja", "other"));
    }

    #[test]
    fn field_boundaries_are_not_ambiguous() {
        // Without a separator these two would hash the same byte stream.
        assert_ne!(
            GlossKey::new("ab", "c", "ja", "text"),
            GlossKey::new("a", "bc", "ja", "text")
        );
    }

    #[test]
    fn hex_is_64_lowercase_digits() {
        let hex = GlossKey::new("m", "en", "ja", "text").to_hex();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }
}
