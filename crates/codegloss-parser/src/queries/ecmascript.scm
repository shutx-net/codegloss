; Every comment in a JavaScript, TypeScript or TSX file.
;
; Matching syntax nodes rather than text is the whole point of using
; Tree-sitter here: a `//` inside a string literal such as
; "https://example.com" is part of the string node and never matches.
;
; One query for the three languages: the TypeScript and TSX grammars are
; generated from JavaScript's, so `(comment)` is the same node in all of them.
; Like Go's, that node covers both `//` and `/* */` and carries no doc-comment
; marker, so every comment reaches `RawComment` as a plain one. A JSDoc block is
; told apart by its `/**` opener, which `codegloss-core`'s `docblock` reads off
; the text.
;
; `(html_comment)` is deliberately not captured. It is the `<!-- -->` a browser
; of the 1990s needed around an inline script, it is not a comment anybody
; writes today, and its markers are not the ones `docblock` strips.
(comment) @comment
