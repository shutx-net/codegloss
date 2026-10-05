; Every comment in a Zig file.
;
; Matching syntax nodes rather than text is the whole point of using
; Tree-sitter here: a `//` inside a string literal such as
; "https://example.com", or on a `\\` line of a multiline string, is part of
; the string node and never matches.
;
; One capture, and one node behind it: the grammar's only comment rule is
; `token(seq('//', /.*/))` (grammar.js of tree-sitter-zig 1.1.2), so `//`,
; `///`, `//!` and `////` are the same node. Zig has no block comment, and the
; grammar hands over no doc marker, so the marker stays in the text and
; `RawComment` reads it off there, spelled the way the registry spells it
; (`LineDocMarkers` in `languages.rs`).
(comment) @comment
