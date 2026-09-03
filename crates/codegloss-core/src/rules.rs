//! What the shape of a comment means in the language it was written in.
//!
//! This crate never learns which languages exist. It owns the vocabulary of
//! comment shapes; `codegloss-parser` owns the registry and says which language
//! speaks which. Adding Python or Java is then a change to that registry alone,
//! and the dependency arrow stays pointed one way.

use serde::{Deserialize, Serialize};

/// The set of shape rules one comment was written under.
///
/// Named for the shape and never for the language: two languages that mark an
/// example the same way share a set, and sharing a set is what lets them share
/// cache entries. A variant named `Go` would hide that.
///
/// It is hashed into every [`GlossKey`](crate::GlossKey) beside the pipeline
/// version, so one sentence read under two sets cannot collide - and
/// `// Returns the user.` is a comment in both of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CommentRules {
    /// An example is marked with a Markdown fence, and indentation on its own
    /// says nothing. Rust, and everything CodeGloss read before Go.
    Fenced,
    /// A run of indented lines is an example. Go's doc comments, which carry no
    /// fence at all (`docs/model-runtime-notes.md` §16).
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
            Self::Indented => "indented",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag is written into cache keys that outlive the binary, so it is
    /// pinned here rather than derived from the variant.
    #[test]
    fn the_tag_is_a_storage_format() {
        assert_eq!(CommentRules::Fenced.tag(), "fenced");
        assert_eq!(CommentRules::Indented.tag(), "indented");
        assert_ne!(CommentRules::Fenced.tag(), CommentRules::Indented.tag());
    }
}
