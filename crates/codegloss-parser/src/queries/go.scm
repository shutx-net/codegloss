; Every comment in a Go file.
;
; Matching syntax nodes rather than text is the whole point of using
; Tree-sitter here: a `//` inside a string literal such as
; "https://example.com" is part of the string node and never matches.
;
; One capture, not two: Go's grammar has a single `(comment)` node for both
; `//` and `/* */`, and no doc-comment marker of its own. So every Go comment
; reaches `RawComment` as a plain one, which is right - a Go doc comment is
; told from any other comment by what it sits above, never by how it is
; written.
(comment) @comment
