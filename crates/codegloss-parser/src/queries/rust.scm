; Every comment in a Rust file, doc comments included.
;
; Matching syntax nodes rather than text is the whole point of using
; Tree-sitter here: a `//` inside a string literal such as
; "https://example.com" is part of the string node and never matches.
(line_comment) @comment
(block_comment) @comment
