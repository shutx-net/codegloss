//! In-memory mirror of the documents the client has open.
//!
//! The server negotiates `TextDocumentSyncKind::FULL`, so a change notification
//! always carries the whole buffer and no incremental patching is needed here.

use dashmap::DashMap;
use tower_lsp_server::ls_types::Uri;

/// The client's current view of one open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentState {
    /// Language id as reported by the client (`rust`, `java`, ...). Selects the
    /// Tree-sitter grammar in later phases.
    pub language_id: String,
    pub version: i32,
    pub text: String,
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
        self.documents.insert(
            uri,
            DocumentState {
                language_id,
                version,
                text,
            },
        );
    }

    /// Replaces the contents of an open document.
    ///
    /// A change for a document that was never opened is dropped: without the
    /// language id from `didOpen` there is no grammar to parse it with.
    pub fn update(&self, uri: &Uri, version: i32, text: String) {
        if let Some(mut document) = self.documents.get_mut(uri) {
            document.version = version;
            document.text = text;
        }
    }

    pub fn close(&self, uri: &Uri) {
        self.documents.remove(uri);
    }

    /// Returns a snapshot of the document. Cloning keeps the map unlocked while
    /// a background translation job works on the text.
    pub fn snapshot(&self, uri: &Uri) -> Option<DocumentState> {
        self.documents.get(uri).map(|entry| entry.value().clone())
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn uri() -> Uri {
        Uri::from_str("file:///tmp/main.rs").expect("valid file uri")
    }

    #[test]
    fn open_then_snapshot_returns_the_text() {
        let store = DocumentStore::new();
        store.open(uri(), "rust".to_owned(), 1, "// hello".to_owned());

        let document = store.snapshot(&uri()).expect("document is open");
        assert_eq!(document.language_id, "rust");
        assert_eq!(document.version, 1);
        assert_eq!(document.text, "// hello");
    }

    #[test]
    fn update_replaces_the_whole_text() {
        let store = DocumentStore::new();
        store.open(uri(), "rust".to_owned(), 1, "// hello".to_owned());
        store.update(&uri(), 2, "// goodbye".to_owned());

        let document = store.snapshot(&uri()).expect("document is open");
        assert_eq!(document.version, 2);
        assert_eq!(document.text, "// goodbye");
    }

    #[test]
    fn update_of_an_unopened_document_is_ignored() {
        let store = DocumentStore::new();
        store.update(&uri(), 1, "// hello".to_owned());
        assert!(store.is_empty());
    }

    #[test]
    fn close_removes_the_document() {
        let store = DocumentStore::new();
        store.open(uri(), "rust".to_owned(), 1, "// hello".to_owned());
        store.close(&uri());
        assert_eq!(store.len(), 0);
        assert!(store.snapshot(&uri()).is_none());
    }
}
