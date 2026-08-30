//! In-memory mirror of the documents the client has open.
//!
//! The server negotiates `TextDocumentSyncKind::FULL`, so a change notification
//! always carries the whole buffer and no incremental patching is needed here.
//! Each stored buffer keeps the comment blocks extracted from it, which is what
//! the request handlers answer from.

use codegloss_core::CommentBlock;
use codegloss_parser::{SupportedLanguage, extract_comment_blocks};
use dashmap::DashMap;
use tower_lsp_server::ls_types::{Position, Range, Uri};

/// The client's current view of one open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentState {
    /// Language id as reported by the client (`rust`, `java`, ...). Selects the
    /// Tree-sitter grammar.
    pub language_id: String,
    pub version: i32,
    pub text: String,
    /// Comments found in `text`, sorted by position. Empty for a language
    /// CodeGloss cannot parse, which is not an error: the document simply has
    /// nothing to gloss.
    pub blocks: Vec<CommentBlock>,
}

/// A comment block together with the editor coordinates it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBlockHit {
    pub block: CommentBlock,
    pub range: Range,
}

/// Concurrent map of open documents.
///
/// Translation runs on background tasks that need to read the buffer while the
/// LSP handlers keep serving requests, hence a `DashMap` rather than a `Mutex`.
#[derive(Debug, Default)]
pub struct DocumentStore {
    documents: DashMap<Uri, DocumentState>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self, uri: Uri, language_id: String, version: i32, text: String) {
        let blocks = extract(&uri, &language_id, &text);
        self.documents.insert(
            uri,
            DocumentState {
                language_id,
                version,
                text,
                blocks,
            },
        );
    }

    /// Replaces the contents of an open document and re-extracts its comments.
    ///
    /// A change for a document that was never opened is dropped: without the
    /// language id from `didOpen` there is no grammar to parse it with.
    pub fn update(&self, uri: &Uri, version: i32, text: String) {
        let Some(mut document) = self.documents.get_mut(uri) else {
            return;
        };
        document.blocks = extract(uri, &document.language_id, &text);
        document.version = version;
        document.text = text;
    }

    pub fn close(&self, uri: &Uri) {
        self.documents.remove(uri);
    }

    /// Returns a snapshot of the document. Cloning keeps the map unlocked while
    /// a background translation job works on the text.
    pub fn snapshot(&self, uri: &Uri) -> Option<DocumentState> {
        self.documents.get(uri).map(|entry| entry.value().clone())
    }

    /// The comment block `position` points into, if any.
    ///
    /// The test is on byte offsets rather than on line numbers, so a position
    /// in the code part of `let x = 1; // note` yields nothing while a position
    /// in the comment part of the same line yields the note.
    pub fn comment_block_at(&self, uri: &Uri, position: Position) -> Option<CommentBlockHit> {
        let document = self.documents.get(uri)?;
        let offset = byte_offset_at(&document.text, position)?;
        let block = document
            .blocks
            .iter()
            .find(|block| (block.start_byte..block.end_byte).contains(&offset))?;

        Some(CommentBlockHit {
            range: Range {
                start: position_at(&document.text, block.start_byte)?,
                end: position_at(&document.text, block.end_byte)?,
            },
            block: block.clone(),
        })
    }

    /// Runs `read` over the comment blocks of a document, without copying them.
    ///
    /// [`Self::snapshot`] would do as well, but it clones the whole buffer; a
    /// request that only needs the blocks - one code lens per comment, on every
    /// keystroke, for a file with hundreds of them - should not pay for the
    /// text as well.
    ///
    /// IMPORTANT: `read` runs while the document is locked for reading. It must
    /// not touch this store again, and it must not block.
    pub fn with_blocks<T>(&self, uri: &Uri, read: impl FnOnce(&[CommentBlock]) -> T) -> Option<T> {
        self.documents.get(uri).map(|entry| read(&entry.blocks))
    }

    /// Every comment of a document, in source order, each exactly as the file
    /// has it.
    ///
    /// This is what gets queued for translation. `raw` rather than `text`: the
    /// pre-processing reads the structure of a block - its blank lines, its
    /// `@return` lines, its fenced examples - off the comment as written, and
    /// `text` has already been flattened into a single line by then.
    ///
    /// The whole document goes at once, even when a single hover prompted it: a
    /// job carrying only part of a document could not safely replace an earlier
    /// job for the same one.
    pub fn comment_sources(&self, uri: &Uri) -> Vec<String> {
        self.documents.get(uri).map_or_else(Vec::new, |document| {
            document
                .blocks
                .iter()
                .map(|block| block.raw.clone())
                .collect()
        })
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

/// Extracts the comments of a document, turning every failure into "no
/// comments". A grammar CodeGloss does not have, or one that fails to load,
/// must not stop the editor from working.
fn extract(uri: &Uri, language_id: &str, text: &str) -> Vec<CommentBlock> {
    let Some(language) = SupportedLanguage::from_lsp_language_id(language_id) else {
        tracing::debug!(
            language_id,
            "no grammar for this language, nothing to gloss"
        );
        return Vec::new();
    };

    match extract_comment_blocks(text, language) {
        Ok(blocks) => blocks,
        Err(error) => {
            tracing::warn!(uri = uri.as_str(), %error, "comment extraction failed");
            Vec::new()
        }
    }
}

/// Byte offset of an LSP position within `text`.
///
/// IMPORTANT: `Position::character` counts UTF-16 code units, which is what the
/// protocol defaults to. Treating it as a byte or a `char` index puts every
/// hover on a line containing Japanese or an emoji in the wrong place. Positions
/// past the end of their line are clamped to the line end, as the spec requires.
fn byte_offset_at(text: &str, position: Position) -> Option<usize> {
    let mut line_start = 0;
    for _ in 0..position.line {
        line_start += text[line_start..].find('\n')? + 1;
    }

    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |offset| line_start + offset);

    let mut utf16 = 0;
    for (offset, character) in text[line_start..line_end].char_indices() {
        if utf16 >= position.character {
            return Some(line_start + offset);
        }
        utf16 += character.len_utf16() as u32;
    }
    Some(line_end)
}

/// The inverse of [`byte_offset_at`]: editor coordinates for a byte offset.
fn position_at(text: &str, offset: usize) -> Option<Position> {
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);

    Some(Position {
        line: before.matches('\n').count() as u32,
        character: before[line_start..].encode_utf16().count() as u32,
    })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const RUST: &str = "// Return the cached user.\nfn find_user() {}\n";

    fn uri() -> Uri {
        Uri::from_str("file:///tmp/main.rs").expect("valid file uri")
    }

    fn store() -> DocumentStore {
        let store = DocumentStore::new();
        store.open(uri(), "rust".to_owned(), 1, RUST.to_owned());
        store
    }

    #[test]
    fn open_then_snapshot_returns_the_text() {
        let document = store().snapshot(&uri()).expect("document is open");
        assert_eq!(document.language_id, "rust");
        assert_eq!(document.version, 1);
        assert_eq!(document.text, RUST);
    }

    #[test]
    fn opening_extracts_the_comments() {
        let document = store().snapshot(&uri()).expect("document is open");
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].text, "Return the cached user.");
    }

    #[test]
    fn an_unsupported_language_yields_no_blocks() {
        let store = DocumentStore::new();
        store.open(uri(), "plaintext".to_owned(), 1, RUST.to_owned());

        let document = store.snapshot(&uri()).expect("document is open");
        assert!(document.blocks.is_empty());
    }

    #[test]
    fn update_replaces_the_text_and_the_blocks() {
        let store = store();
        store.update(&uri(), 2, "// Changed.\n".to_owned());

        let document = store.snapshot(&uri()).expect("document is still open");
        assert_eq!(document.version, 2);
        assert_eq!(document.text, "// Changed.\n");
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].text, "Changed.");
    }

    #[test]
    fn update_of_an_unopened_document_is_ignored() {
        let store = DocumentStore::new();
        store.update(&uri(), 1, RUST.to_owned());
        assert!(store.is_empty());
    }

    /// The markers are part of what is queued: the gloss of a `/** */` block is
    /// not the gloss of the same sentence written as `//`.
    #[test]
    fn comment_sources_lists_every_comment_as_written_in_order() {
        let store = DocumentStore::new();
        let text = "// First.\nfn f() {}\n/// Second.\nfn g() {}\n";
        store.open(uri(), "rust".to_owned(), 1, text.to_owned());

        assert_eq!(
            store.comment_sources(&uri()),
            vec!["// First.".to_owned(), "/// Second.".to_owned()]
        );
    }

    #[test]
    fn comment_sources_of_an_unknown_document_is_empty() {
        assert!(DocumentStore::new().comment_sources(&uri()).is_empty());
    }

    #[test]
    fn with_blocks_sees_the_blocks_of_an_open_document() {
        let texts = store()
            .with_blocks(&uri(), |blocks| {
                blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
            })
            .expect("document is open");
        assert_eq!(texts, vec!["Return the cached user.".to_owned()]);
    }

    /// A document nobody opened is told apart from one without comments: the
    /// first has no answer, the second has an empty one.
    #[test]
    fn with_blocks_of_an_unknown_document_returns_nothing() {
        assert_eq!(
            DocumentStore::new().with_blocks(&uri(), <[CommentBlock]>::len),
            None
        );
    }

    #[test]
    fn close_removes_the_document() {
        let store = store();
        store.close(&uri());
        assert_eq!(store.len(), 0);
        assert!(store.snapshot(&uri()).is_none());
    }

    #[test]
    fn a_position_inside_a_comment_finds_its_block() {
        let hit = store()
            .comment_block_at(&uri(), Position::new(0, 5))
            .expect("the position is inside the comment");

        assert_eq!(hit.block.text, "Return the cached user.");
        assert_eq!(hit.range.start, Position::new(0, 0));
        assert_eq!(hit.range.end, Position::new(0, 26));
    }

    #[test]
    fn a_position_on_a_code_line_finds_nothing() {
        assert!(
            store()
                .comment_block_at(&uri(), Position::new(1, 4))
                .is_none()
        );
    }

    #[test]
    fn a_position_on_the_code_half_of_a_trailing_comment_line_finds_nothing() {
        let store = DocumentStore::new();
        let text = "fn f() {\n    let x = 1; // Why.\n}\n";
        store.open(uri(), "rust".to_owned(), 1, text.to_owned());

        // Column 8 is inside `let x = 1;`, column 20 is inside `// Why.`.
        assert!(
            store
                .comment_block_at(&uri(), Position::new(1, 8))
                .is_none()
        );
        let hit = store
            .comment_block_at(&uri(), Position::new(1, 20))
            .expect("the position is inside the trailing comment");
        assert_eq!(hit.block.text, "Why.");
    }

    #[test]
    fn a_position_in_an_unknown_document_finds_nothing() {
        let other = Uri::from_str("file:///tmp/other.rs").expect("valid file uri");
        assert!(
            store()
                .comment_block_at(&other, Position::new(0, 0))
                .is_none()
        );
    }

    /// Every `character` below is a UTF-16 count, not a byte count: the string
    /// literal takes 3 bytes per Japanese character but only 1 code unit.
    #[test]
    fn positions_on_a_multibyte_line_map_to_the_right_bytes() {
        let text = "let s = \"日本語\"; // Note.\n";
        assert_eq!(byte_offset_at(text, Position::new(0, 0)), Some(0));
        assert_eq!(byte_offset_at(text, Position::new(0, 9)), Some(9));
        // Right after the three Japanese characters: 9 code units in, 9 + 9
        // bytes in.
        assert_eq!(byte_offset_at(text, Position::new(0, 12)), Some(18));
        // Past the end of the line, clamped to the line end.
        assert_eq!(
            byte_offset_at(text, Position::new(0, 999)),
            Some(text.len() - 1)
        );
        // Line past the end of the document.
        assert_eq!(byte_offset_at(text, Position::new(9, 0)), None);
    }

    #[test]
    fn a_hover_on_a_multibyte_line_is_not_shifted() {
        let store = DocumentStore::new();
        let text = "let s = \"日本語\"; // Note.\n";
        store.open(uri(), "rust".to_owned(), 1, text.to_owned());

        // The comment runs from code unit 15 to 23 but from byte 21 to 29, so
        // reading `character` as a byte offset misses it.
        let hit = store
            .comment_block_at(&uri(), Position::new(0, 20))
            .expect("the position is inside the comment");
        assert_eq!(hit.block.text, "Note.");
        assert_eq!(hit.range.start, Position::new(0, 15));
        assert_eq!(hit.range.end, Position::new(0, 23));
    }

    #[test]
    fn positions_round_trip_through_byte_offsets() {
        let text = "// 日本語\nfn f() {}\n";
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            let position = position_at(text, offset).expect("offset is in range");
            assert_eq!(
                byte_offset_at(text, position),
                Some(offset),
                "offset {offset} did not round-trip"
            );
        }
    }
}
