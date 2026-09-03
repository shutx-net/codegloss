//! Prints the comment blocks of the source files named on the command line.
//!
//! Every measurement in `docs/model-runtime-notes.md` §7, §9 and §12 is taken
//! over a corpus of comments pulled out of real source files, and until this
//! existed the pulling was done by a throwaway script that nobody kept. A
//! number measured on a corpus that cannot be rebuilt is a number nobody can
//! check, so the extractor is part of the repository even though the corpora
//! themselves mostly are not (third-party comments are third-party works -
//! AGENTS.md, "ライセンス").
//!
//! ```sh
//! cargo run -p codegloss-parser --example extract -- --lang rust \
//!   crates/codegloss-lsp/src/translation.rs crates/codegloss-core/src/cache.rs
//! ```
//!
//! The language is named, never guessed from the extension. That is the rule
//! the whole pipeline follows: a buffer is whatever the editor says it is
//! (`SupportedLanguage::from_lsp_language_id`), and an extension is only one of
//! the inputs to that guess. A corpus measured under the wrong grammar would be
//! a number nobody could check either.
//!
//! Output is [`CommentBlock::raw`] - the comment exactly as it stands in the
//! file, markers and all - with a `%%%` line between blocks. That is the format
//! `examples/probe.rs` and `codegloss-translator/tests/pipelines.rs` read, so
//! the output of this can be piped straight into either.
//!
//! Deliberately not included: directory walking, globbing and filtering. The
//! shell already has all three, and a corpus is worth more when the command
//! that built it fits on one line of a document.
//!
//! [`CommentBlock::raw`]: codegloss_core::CommentBlock::raw

use std::path::PathBuf;
use std::process::ExitCode;

use codegloss_parser::{SupportedLanguage, extract_comment_blocks};

const USAGE: &str = "usage: extract [--lang rust|go] <file>...";

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1).peekable();
    let mut language = SupportedLanguage::Rust;
    if arguments.peek().is_some_and(|first| first == "--lang") {
        arguments.next();
        let Some(named) = arguments.next() else {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        };
        match SupportedLanguage::from_lsp_language_id(&named) {
            Some(chosen) => language = chosen,
            None => {
                eprintln!("unknown language {named:?}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let paths: Vec<PathBuf> = arguments.map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }

    let mut blocks = 0usize;
    for path in &paths {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        };

        let extracted = match extract_comment_blocks(&source, language) {
            Ok(extracted) => extracted,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        };

        for block in extracted {
            if blocks > 0 {
                println!("%%%");
            }
            blocks += 1;
            println!("{}", block.raw);
        }
    }

    eprintln!(
        "{blocks} blocks from {} files as {}",
        paths.len(),
        language.as_str()
    );
    ExitCode::SUCCESS
}
