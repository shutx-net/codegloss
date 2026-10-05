//! What the shape of a comment means in the language it was written in.
//!
//! This crate never learns which languages exist. It owns the vocabulary of
//! comment shapes; `codegloss-parser` owns the registry and says which language
//! speaks which. Adding Python or Java is then a change to that registry alone,
//! and the dependency arrow stays pointed one way.

use serde::{Deserialize, Serialize};

/// The set of shape rules one comment was written under.
///
/// Named for the shape and never for the language: two languages that shape a
/// comment the same way - the same mark for an example, the same answer on doc
/// tags - share a set, and sharing a set is what lets them share cache
/// entries. A variant named `Go` would hide that.
///
/// It is hashed into every [`GlossKey`](crate::GlossKey) beside the pipeline
/// version, so one sentence read under two sets cannot collide - and
/// `// Returns the user.` is a comment in both of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CommentRules {
    /// An example is marked with a Markdown fence, and indentation on its own
    /// says nothing. A line that opens with `@` and a word is a doc tag - a
    /// block tag of JSDoc or Javadoc, whose vocabulary is `docblock`'s
    /// `TagArguments`. JavaScript, TypeScript and TSX.
    Fenced,
    /// An example is marked with a Markdown fence, exactly as under
    /// [`Self::Fenced`], and a line that opens with `@` and a word is prose,
    /// never a doc tag.
    ///
    /// Rustdoc and Zig's autodoc are Markdown and nothing else: neither has a
    /// block tag, so reading one off the line can only be wrong. Zig is where
    /// that showed - `@intCast` is a builtin function, and one that opened a
    /// line split its paragraph there and handed the engine the two halves
    /// (`docs/model-runtime-notes.md` §19.6). Rust and Zig.
    FencedUntagged,
    /// A run of indented lines is an example. Go's doc comments, which carry no
    /// fence at all (`docs/model-runtime-notes.md` §16).
    ///
    /// A line that opens with `@` and a word is a doc tag here, as it is under
    /// [`Self::Fenced`]. Go's doc comments have no block tags either, but
    /// whether Go should stop reading one is a decision of its own, and it was
    /// not taken with Rust's.
    Indented,
}

impl CommentRules {
    /// The identity hashed into a [`GlossKey`](crate::GlossKey).
    ///
    /// A fixed string rather than the variant's position, because this is a
    /// storage format: a cache directory outlives the build that wrote it, and
    /// a variant inserted above another one would silently hand every gloss to
    /// the wrong rules.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Fenced => "fenced",
            Self::FencedUntagged => "fenced-untagged",
            Self::Indented => "indented",
        }
    }

    /// Whether a line that opens with `@` and a word is a doc tag - the line
    /// that `docblock` gives a lead of its own and reads an `@example` off.
    ///
    /// Asked of the rules rather than passed down as a flag, so that no caller
    /// can hand `docblock` an answer that disagrees with the set the comment
    /// was read under. A `match` rather than a comparison against the one set
    /// that answers no: a set added later has to say which it is, and `!=`
    /// would say it for whoever forgot to.
    pub(crate) fn reads_doc_tags(self) -> bool {
        match self {
            Self::Fenced | Self::Indented => true,
            Self::FencedUntagged => false,
        }
    }

    /// Reads back what [`Self::tag`] wrote, or `None` for a tag no set claims.
    ///
    /// A cache directory is not the only thing that outlives the build that
    /// wrote it: the measurement corpora of `docs/model-runtime-notes.md` §12
    /// are files too, and a corpus that does not say which set it was read
    /// under is scored under the wrong one. Reading is here beside writing so
    /// that the spellings exist once - `codegloss-parser`'s `corpus` module
    /// puts the tag on the file, and this is what takes it off again.
    ///
    /// `None` rather than a default: an unrecognised tag is a corpus written by
    /// a newer build, and guessing at it would score it silently.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rules| rules.tag() == tag)
    }

    /// Every set. Private: nothing outside needs to enumerate the vocabulary,
    /// and [`Self::from_tag`] needs one list rather than a second copy of the
    /// spellings in a `match`.
    const ALL: [Self; 3] = [Self::Fenced, Self::FencedUntagged, Self::Indented];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag is written into cache keys that outlive the binary, so it is
    /// pinned here rather than derived from the variant.
    #[test]
    fn the_tag_is_a_storage_format() {
        assert_eq!(CommentRules::Fenced.tag(), "fenced");
        assert_eq!(CommentRules::FencedUntagged.tag(), "fenced-untagged");
        assert_eq!(CommentRules::Indented.tag(), "indented");
        // Two sets writing one tag would read each other's glosses back.
        for (index, rules) in CommentRules::ALL.iter().enumerate() {
            for other in &CommentRules::ALL[index + 1..] {
                assert_ne!(rules.tag(), other.tag(), "{rules:?} and {other:?}");
            }
        }
    }

    #[test]
    fn every_tag_reads_back_as_the_set_that_wrote_it() {
        for rules in CommentRules::ALL {
            assert_eq!(
                CommentRules::from_tag(rules.tag()),
                Some(rules),
                "{rules:?} does not survive its own tag"
            );
        }
    }

    /// A tag is matched as it is written. Anything else - a set this build does
    /// not have, a spelling that only differs in case, an empty line - is not a
    /// set, and saying so is what keeps a corpus from being scored under rules
    /// it was not extracted under.
    #[test]
    fn an_unrecognised_tag_is_not_a_set() {
        for tag in [
            "",
            " ",
            "Fenced",
            "FENCED",
            "indent",
            "python",
            "fenced ",
            "fenced_untagged",
            "untagged",
            "Fenced-Untagged",
        ] {
            assert_eq!(
                CommentRules::from_tag(tag),
                None,
                "{tag:?} was accepted as a set"
            );
        }
    }
}
