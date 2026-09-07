//! Prints every language this build reads comments from together with the
//! `languageId`s it answers to, one pair per line, separated by a tab.
//!
//! ```text
//! javascript<TAB>javascript
//! javascript<TAB>javascriptreact
//! ```
//!
//! It exists so that CI can compare that against the lists kept outside this
//! workspace: `editors/zed/extension.toml` names the languages Zed attaches the
//! server to, and `editors/vscode/package.json` names them again as VS Code's
//! `onLanguage:` activation events. Neither build sees this one, so adding a
//! language to one alone fails silently in both directions - the server parses
//! a language the editor never attaches it to, or the editor attaches it and
//! `SupportedLanguage::from_lsp_language_id` answers `None` and the buffer is
//! treated as having no comments (Issue #63).
//!
//! Two columns rather than one because the two lists are not the same list.
//! Zed spells a language the way this registry does, so the names column is
//! what it is compared against; VS Code spells two of them differently
//! (`typescriptreact`, `javascriptreact`), so the ids column is what covers it.
//! Every id has to be claimed by one of the two editors, or the server would be
//! answering to a name nothing sends.
//!
//! Printed by the compiler's own list rather than scraped out of
//! `src/languages.rs`, because a check that reads the text of the registry
//! compares CI against the spelling of the code and not against what the binary
//! does. The names differ in case between the files (`rust` here, `Rust` in
//! `extension.toml`), so the comparisons are on lower-cased sets.
//!
//! ```sh
//! cargo run -q -p codegloss-parser --example languages
//! ```

use codegloss_parser::SupportedLanguage;

fn main() {
    for language in SupportedLanguage::ALL {
        for id in language.lsp_language_ids() {
            println!("{}\t{id}", language.as_str());
        }
    }
}
