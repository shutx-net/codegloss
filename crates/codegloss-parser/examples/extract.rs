//! Prints the comment blocks of the Rust files named on the command line.
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
//! cargo run -p codegloss-parser --example extract -- \
//!   crates/codegloss-lsp/src/translation.rs crates/codegloss-core/src/cache.rs
//! ```
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

fn main() -> ExitCode {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: extract <file.rs>...");
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

        // Rust is the only language the parser reads today, so the extension is
        // not consulted: a file named on the command line is one the caller
        // means to parse as Rust.
        let extracted = match extract_comment_blocks(&source, SupportedLanguage::Rust) {
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

    eprintln!("{blocks} blocks from {} files", paths.len());
    ExitCode::SUCCESS
}
