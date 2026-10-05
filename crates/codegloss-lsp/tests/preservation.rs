//! What Issue #1 asks for, checked through the server rather than through the
//! core crate: a comment goes in, a gloss comes out, and every span that must
//! not be translated is still spelled exactly as it was.
//!
//! The engine here is the real [`PassthroughTranslator`], which returns its
//! input. That is what makes the assertions exact: anything the answers differ
//! in is the pre-processing or the post-processing, because nothing else in the
//! pipeline is allowed to change a character.
//!
//! Swapping the engine for candle re-runs these fixtures first
//! when a gloss then comes out wrong: if they still hold, the model is at fault
//! and not this code.

mod harness;

use std::sync::{Arc, Mutex};

use codegloss_core::Segment;
use codegloss_translator::Translator;
use harness::Harness;

const DOCUMENT_URI: &str = "file:///tmp/codegloss/preservation.rs";

/// Every pattern Issue #1 lists, in one file:
/// a `TODO:` prefix, identifiers of each shape, inline code, a URL, and a
/// Javadoc block with `@param` / `@return` / `@throws`, plus the comment of
/// more than one sentence Issue #49 was about.
///
/// The file is Rust, and Rust's rules have no doc tags, so the Javadoc block
/// here is read with its tag lines as prose. Its tags are read as tags in
/// [`JAVADOC_TEXT`], under a language that has them.
const DOCUMENT_TEXT: &str = concat!(
    "// TODO: cache the result of find_user before UserRepository::load() runs.\n",
    "fn find_user() {}\n",
    "\n",
    "/// Returns `UserDetails` when authentication succeeds.\n",
    "///\n",
    "/// See https://example.com/docs/auth for the protocol.\n",
    "fn authenticate() {}\n",
    "\n",
    "/**\n",
    " * Returns the currently authenticated user.\n",
    " *\n",
    " * @param id the id to look up\n",
    " * @return authenticated user\n",
    " * @throws AuthenticationException if authentication failed\n",
    " */\n",
    "fn current_user() {}\n",
    "\n",
    "/// Returns the cached user. Nothing is written back.\n",
    "fn cached_user() {}\n",
);

/// Brings the server up on [`DOCUMENT_TEXT`] and waits for its glosses.
///
/// The engine that ships today is the passthrough, and the harness answers
/// the refresh requests so that the worker never waits on one.
fn glossed_document() -> Harness {
    let mut server = Harness::new();
    let batches = server.batches();
    let seen = batches.count();

    server.initialize();
    server.did_open(DOCUMENT_URI, "rust", DOCUMENT_TEXT);

    server.settle(seen);
    server
}

const JAVADOC_URI: &str = "file:///tmp/codegloss/preservation.ts";

/// The Javadoc block of [`DOCUMENT_TEXT`] in a TypeScript file of its own.
///
/// TypeScript because JSDoc writes block tags, and a tag line is one only
/// under rules that say a comment has them - Rust's say it has none
/// (`CommentRules::FencedUntagged`), because Rustdoc writes none. JSDoc's
/// rules read Javadoc's spelling too: the two share the `@tag` shape, and the
/// bare exception type after `@throws` fills the slot JSDoc writes a type in.
const JAVADOC_TEXT: &str = concat!(
    "/**\n",
    " * Returns the currently authenticated user.\n",
    " *\n",
    " * @param id the id to look up\n",
    " * @return authenticated user\n",
    " * @throws AuthenticationException if authentication failed\n",
    " */\n",
    "export function currentUser() {}\n",
);

/// Brings the server up on [`JAVADOC_TEXT`] and waits for its glosses, the way
/// [`glossed_document`] does for the Rust file.
fn glossed_javadoc() -> Harness {
    let mut server = Harness::new();
    let batches = server.batches();
    let seen = batches.count();

    server.initialize();
    server.did_open(JAVADOC_URI, "typescript", JAVADOC_TEXT);

    server.settle(seen);
    server
}

/// The gloss a hover shows, without the quoted English underneath it.
fn gloss_at(server: &mut Harness, line: u32, character: u32) -> String {
    gloss_at_in(server, DOCUMENT_URI, line, character)
}

fn gloss_at_in(server: &mut Harness, uri: &str, line: u32, character: u32) -> String {
    let value = markup_at_in(server, uri, line, character);
    let (gloss, quoted) = value
        .split_once("\n\n> ")
        .expect("a finished gloss quotes its source");
    assert!(!quoted.is_empty(), "the English is quoted underneath");
    gloss.to_owned()
}

/// The Markdown a hover answers with, exactly as the editor receives it.
fn markup_at_in(server: &mut Harness, uri: &str, line: u32, character: u32) -> String {
    let response = server.hover(uri, line, character);

    response["result"]["contents"]["value"]
        .as_str()
        .expect("a hover carries markdown")
        .to_owned()
}

fn lens_titles(server: &mut Harness) -> Vec<(u64, String)> {
    lens_titles_in(server, DOCUMENT_URI)
}

fn lens_titles_in(server: &mut Harness, uri: &str) -> Vec<(u64, String)> {
    let response = server.code_lens(uri);

    response["result"]
        .as_array()
        .expect("the answer is a list of lenses")
        .iter()
        .map(|lens| {
            (
                lens["range"]["start"]["line"]
                    .as_u64()
                    .expect("a lens points at a line"),
                lens["command"]["title"]
                    .as_str()
                    .expect("a lens carries a title")
                    .to_owned(),
            )
        })
        .collect()
}

/// A `//` comment: the `TODO:` prefix and all three shapes of identifier come
/// back byte for byte.
#[test]
fn a_line_comment_keeps_its_prefix_and_its_identifiers() {
    let mut server = glossed_document();

    assert_eq!(
        gloss_at(&mut server, 0, 5),
        "TODO: cache the result of find_user before UserRepository::load() runs."
    );
}

#[test]
fn a_doc_comment_keeps_its_inline_code() {
    let mut server = glossed_document();

    assert_eq!(
        gloss_at(&mut server, 3, 10),
        "Returns `UserDetails` when authentication succeeds."
    );
}

#[test]
fn a_doc_comment_keeps_its_url() {
    let mut server = glossed_document();

    assert_eq!(
        gloss_at(&mut server, 5, 10),
        "See https://example.com/docs/auth for the protocol."
    );
}

/// The Javadoc block of Issue #1: the paragraph, the blank line and one line
/// per tag are all still there, and the tags and the exception type are
/// untouched.
///
/// The two trailing spaces are Markdown's hard line break. Without them the
/// editor would run the three tag lines together into one paragraph.
///
/// Read out of [`JAVADOC_TEXT`], where the tags are tags, rather than out of
/// the Rust file, where they are prose.
#[test]
fn a_javadoc_block_keeps_its_line_structure_and_its_tags() {
    let mut server = glossed_javadoc();

    assert_eq!(
        gloss_at_in(&mut server, JAVADOC_URI, 1, 10),
        concat!(
            "Returns the currently authenticated user.\n",
            "\n",
            "@param id the id to look up  \n",
            "@return authenticated user  \n",
            "@throws AuthenticationException if authentication failed",
        )
    );
}

/// Two sentences in one comment come back with the space between them.
///
/// The defect of Issue #49, at the level the reader meets it: `join_sentences`
/// suppressed the space after an ASCII full stop as well as after a `。`, so a
/// comment of two sentences was shown as `...user.Nothing...`. The engine here
/// is the one that produces it in production - a server with no model pack
/// answers with its input, which is English.
#[test]
fn a_comment_of_two_sentences_keeps_the_space_between_them() {
    let mut server = glossed_document();

    assert_eq!(
        gloss_at(&mut server, 17, 10),
        "Returns the cached user. Nothing is written back."
    );
}

/// The same glosses reach the other display mode. A lens is one line high, so
/// the Javadoc block is folded and cut - that is the code lens doing it, and everything
/// that fits is still exact.
#[test]
fn the_lenses_carry_the_same_glosses() {
    let mut server = glossed_document();
    let titles = lens_titles(&mut server);

    assert_eq!(
        titles[..3],
        [
            (
                0,
                "TODO: cache the result of find_user before UserRepository::load() runs."
                    .to_owned()
            ),
            (
                3,
                "Returns `UserDetails` when authentication succeeds.".to_owned()
            ),
            (
                5,
                "See https://example.com/docs/auth for the protocol.".to_owned()
            ),
        ]
    );

    let (line, javadoc) = &titles[3];
    assert_eq!(*line, 8, "the lens sits on the comment's own first line");
    assert!(
        javadoc.starts_with(
            "Returns the currently authenticated user. @param id the id to look up @return authenticated user @throws "
        ),
        "{javadoc:?}"
    );
}

/// Nothing in the document was left without a gloss: a pattern the
/// pre-processing choked on would show up as a lens still saying "translating".
#[test]
fn every_comment_of_the_document_is_glossed() {
    let mut server = glossed_document();
    let titles = lens_titles(&mut server);

    assert_eq!(titles.len(), 5);
    for (line, title) in titles {
        assert!(
            title != codegloss_lsp::code_lens::PENDING_TITLE,
            "the comment on line {line} was never glossed"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue #53: a doctest is code, and code is not translated.
// ---------------------------------------------------------------------------

const DOCTEST_URI: &str = "file:///tmp/codegloss/doctest.rs";

/// The snippet Issue #53 is written around, with the two shapes that used to
/// tear it apart: blank `///` lines between the paragraphs and brace-only lines
/// inside the example.
const DOCTEST_TEXT: &str = concat!(
    "/// Writes the whole buffer.\n",
    "///\n",
    "/// # Examples\n",
    "///\n",
    "/// ```\n",
    "/// let mut pos = 0;\n",
    "/// while pos < data.len() {\n",
    "///     let n = writer.write(&data[pos..])?;\n",
    "///     pos += n;\n",
    "/// }\n",
    "/// Ok(())\n",
    "/// ```\n",
    "pub fn write_all() {}\n",
);

/// An engine that records what it is asked and marks what it answers.
///
/// Both halves are load-bearing. [`PassthroughTranslator`] returns its input,
/// so with it a doctest that went through the model and one that never did look
/// exactly alike; the recording is what tells them apart. The `[ja] ` mark then
/// says, in the finished gloss, which lines came from the engine and which came
/// from the source.
struct RecordingEngine {
    asked: Mutex<Vec<String>>,
}

impl RecordingEngine {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .expect("the recorder is not poisoned")
            .clone()
    }
}

impl Translator for RecordingEngine {
    fn translate(&self, segments: &[Segment]) -> anyhow::Result<Vec<String>> {
        let mut asked = self.asked.lock().expect("the recorder is not poisoned");
        Ok(segments
            .iter()
            .map(|segment| {
                asked.push(segment.text().to_owned());
                format!("[ja] {}", segment.text())
            })
            .collect())
    }

    fn model_version(&self) -> &str {
        "recording-engine"
    }
}

/// Brings a server up on [`DOCTEST_TEXT`] with a recording engine and waits
/// for its glosses.
fn glossed_doctest() -> (Harness, Arc<RecordingEngine>) {
    let engine = RecordingEngine::new();
    let mut server = Harness::with_engine(Arc::clone(&engine) as Arc<dyn Translator>);
    let batches = server.batches();
    let seen = batches.count();

    server.initialize();
    server.did_open(DOCTEST_URI, "rust", DOCTEST_TEXT);

    server.settle(seen);
    (server, engine)
}

/// The defect of Issue #53, stated where the reader meets it: no line of the
/// example is ever handed to the engine.
///
/// The prose around it still is. Before the parser carried a run through a
/// fence this document reached the model as four blocks with no fence in any of
/// them, and it answered `mut pos = 0 とする。` for `let mut pos = 0;` and
/// `OK()` for `Ok(())`.
#[test]
fn a_doctest_never_reaches_the_engine() {
    let (mut server, engine) = glossed_doctest();
    let asked = engine.asked();

    assert!(
        asked.iter().any(|segment| segment.contains("whole buffer")),
        "the prose is still translated: {asked:?}"
    );
    for fragment in [
        "pos", "writer", "Ok(", "await", "{", "}", "```", "let ", ";",
    ] {
        assert!(
            !asked.iter().any(|segment| segment.contains(fragment)),
            "the engine was asked for {fragment:?}: {asked:?}"
        );
    }

    let titles = lens_titles_in(&mut server, DOCTEST_URI);
    assert_eq!(titles.len(), 3, "{titles:?}");
    assert_eq!(
        titles[2],
        (
            4,
            concat!(
                "``` let mut pos = 0; while pos < data.len() { let n = ",
                "writer.write(&data[pos..])?; pos += n; } Ok(()) ```",
            )
            .to_owned()
        ),
        "the lens on the example carries the code as it was written"
    );
    assert!(
        titles[0].1.starts_with("[ja] "),
        "the prose above it did go through the engine: {:?}",
        titles[0]
    );
}

/// Hover answers on every line of the example, with the code the reader is
/// looking at rather than with a translation of it.
///
/// The fence and the brace-only lines belong to no block at all today, so
/// `comment_block_at` answers `null` on them and the code lines answer with a
/// gloss of their own.
#[test]
fn a_doctest_answers_hover_on_every_one_of_its_lines() {
    let (mut server, _engine) = glossed_doctest();

    let fence = gloss_at_in(&mut server, DOCTEST_URI, 4, 5);
    let code = gloss_at_in(&mut server, DOCTEST_URI, 5, 5);
    let brace = gloss_at_in(&mut server, DOCTEST_URI, 9, 5);

    assert_eq!(fence, code, "every line of the example is one block");
    assert_eq!(code, brace, "every line of the example is one block");
    // The interior indentation is part of the example, not part of the comment
    // syntax: `CommentShape::parse` takes the marker and the single space after
    // it off a fenced line and copies the rest through (Issue #55).
    assert_eq!(
        fence,
        concat!(
            "```\n",
            "let mut pos = 0;\n",
            "while pos < data.len() {\n",
            "    let n = writer.write(&data[pos..])?;\n",
            "    pos += n;\n",
            "}\n",
            "Ok(())\n",
            "```",
        )
    );
    assert!(!fence.contains("[ja]"), "nothing in it was translated");
}

/// The whole hover payload, not just the gloss half of it: the indentation has
/// to survive `gloss_markup` as well as `CommentShape`.
///
/// The two rules that could still lose it live there. `with_hard_breaks`
/// appends CommonMark's hard line break to a line, and inside a fence two
/// trailing spaces are two characters of code - so it leaves fenced lines
/// alone, which is why none of these lines ends in whitespace.
#[test]
fn the_hover_markup_keeps_the_indentation() {
    let (mut server, _engine) = glossed_doctest();

    assert_eq!(
        markup_at_in(&mut server, DOCTEST_URI, 7, 5),
        concat!(
            "```\n",
            "let mut pos = 0;\n",
            "while pos < data.len() {\n",
            "    let n = writer.write(&data[pos..])?;\n",
            "    pos += n;\n",
            "}\n",
            "Ok(())\n",
            "```\n",
            "\n",
            "> ``` let mut pos = 0; while pos < data.len() { ",
            "let n = writer.write(&data[pos..])?; pos += n; } Ok(()) ```",
        )
    );
}
