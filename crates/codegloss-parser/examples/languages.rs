//! Prints the stable name of every language this build reads comments from,
//! one per line.
//!
//! A name, not the whole of a `languageId`: one language answers to several
//! ids, one spelling per client (`SupportedLanguage::lsp_language_ids`). What
//! comes out here is `as_str`, which is always among them and is the one Zed
//! sends - which is exactly what the comparison below needs and no more. A
//! client that spells a language differently needs its own comparison rather
//! than a wider list here, or this output stops matching `extension.toml`.
//!
//! It exists so that CI can compare that list against the one in
//! `editors/zed/extension.toml`. The two live in different Cargo workspaces, so
//! neither build sees the other, and adding a language to one alone fails
//! silently in both directions: the server parses a language Zed never attaches
//! it to, or Zed attaches it and `SupportedLanguage::from_lsp_language_id`
//! answers `None` and the buffer is treated as having no comments (Issue #63).
//!
//! Printed by the compiler's own list rather than scraped out of
//! `src/languages.rs`, because a check that reads the text of the registry
//! compares CI against the spelling of the code and not against what the binary
//! does. The names differ in case between the two files (`rust` here, `Rust`
//! there), so the comparison is on the lower-cased sets.
//!
//! ```sh
//! cargo run -q -p codegloss-parser --example languages
//! ```

use codegloss_parser::SupportedLanguage;

fn main() {
    for language in SupportedLanguage::ALL {
        println!("{}", language.as_str());
    }
}
