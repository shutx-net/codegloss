//! The shape of a comment, and how a translation is poured back into it.
//!
//! A doc comment is not one sentence. It is a paragraph, then a blank line,
//! then a row of `@return` / `@throws` lines, sometimes a Markdown list or a
//! fenced example. Handing all of that to a translator as a single line - which
//! is what [`CommentBlock::text`] is - produces one run-on Japanese sentence
//! with the tags buried inside it.
//!
//! So the block is taken apart into translation units and put back together
//! afterwards:
//!
//! ```text
//! /**                                          |
//!  * Returns the currently authenticated user.  |->  unit 0
//!  *                                            |->  blank
//!  * @return authenticated user                 |->  "@return " + unit 1
//!  * @throws AuthenticationException if ...      |->  "@throws AuthenticationException " + unit 2
//!  */                                          |
//! ```
//!
//! IMPORTANT: this reads [`CommentBlock::raw`], never [`CommentBlock::text`].
//! The parser joins a block's lines with a single space to build `text`, so by
//! then the structure is gone; `raw` is kept beside it for exactly this.
//!
//! Consecutive prose lines are still merged into one unit: a
//! sentence spread over three `//` lines has to reach the engine whole. The
//! unit is then asked for one sentence at a time - see
//! [`split_sentences`](crate::split_sentences) for why - but it stays one unit
//! for masking and for the line it is rebuilt into.
//!
//! What comes back out is the translated prose in the original structure, not a
//! translated copy of the source line. The comment markers (`/**`, ` * `,
//! `///`) are not echoed: a gloss is shown beside the comment it belongs to, so
//! repeating its syntax would only take up the width.
//!
//! Not every language marks an example with a fence. Go's doc comments have no
//! fence at all and indent the example instead, so under
//! [`CommentRules::Indented`] a run of indented lines is copied through the way
//! a fenced one is. Which of the two a comment is read under is the parser's to
//! say - this module owns the vocabulary, never the list of languages.
//!
//! [`CommentBlock::text`]: crate::CommentBlock::text
//! [`CommentBlock::raw`]: crate::CommentBlock::raw

use crate::preserve::{Masked, mask};
use crate::sentence::{engine_form, join_sentences, split_sentences};
use crate::{CommentRules, Segment};

/// Openers of a block comment, longest first so that `/**` is not read as `/*`.
const BLOCK_OPENERS: [&str; 3] = ["/**", "/*!", "/*"];
/// Markers of a line comment, longest first.
const LINE_MARKERS: [&str; 3] = ["///", "//!", "//"];
/// Closer of a block comment.
const BLOCK_CLOSER: &str = "*/";
/// Markdown code fences. Everything between two of them is copied through.
///
/// Back-ticks only, which is narrower than CommonMark: a run of three or more
/// tildes opens a fence there too. In a comment it also draws a rule, and the
/// two cannot be told apart on one line - `~~~~~ Section ~~~~~` is a banner
/// somebody typed, and reading it as a fence swallows every line after it into
/// an example that never closes. The counts say which way to be wrong: over
/// this machine's whole registry (266 crates, 7829 files) not one comment
/// opens a tilde fence, while six lines are caught by mistake, all in `syn`
/// (`docs/model-runtime-notes.md` §15).
const FENCES: [&str; 1] = ["```"];
/// Markdown fences as CommonMark draws them: back-ticks or tildes.
///
/// Wider than [`FENCES`] on purpose, and read by
/// [`opens_or_closes_a_rendered_fence`] alone. The two are not one set and a
/// copy of it - they answer different questions, and that function says which.
const RENDERED_FENCES: [&str; 2] = ["```", "~~~"];
/// Tags whose first argument is an identifier by definition, never prose.
const TAGS_WITH_A_NAME: [&str; 4] = ["@param", "@throws", "@exception", "@arg"];

/// Whether a line opens or closes a fence **CodeGloss copies through**.
///
/// The argument is one line with its comment markers already stripped and
/// trimmed - what [`strip_markers`] returns here, and what a parsed comment's
/// body is on the `codegloss-parser` side. Never the indented form
/// [`after_markers`] returns: a fence written inside a list item would stop
/// being a fence on one side of that and not the other.
///
/// This is public because the parser has to know where a fenced example begins
/// and ends in order to keep one in a single block, and it must not answer that
/// question with a copy of [`FENCES`]: a parser and a [`CommentShape`] that
/// disagreed about what a fence is would put a block boundary in the middle of
/// one, which is the defect the parser's rule exists to prevent. The same
/// reason [`SpanKind`](crate::SpanKind) is exported rather than reimplemented.
///
/// Narrower than CommonMark, deliberately: a run of tildes opens a fence there
/// and does not here (Issue #56, `docs/model-runtime-notes.md` §15). What a
/// Markdown renderer will do with a finished gloss is therefore a different
/// question, asked of [`opens_or_closes_a_rendered_fence`] - which is not a
/// copy of this one gone stale, and takes its argument in the opposite shape.
pub fn opens_or_closes_a_fence(content: &str) -> bool {
    FENCES.iter().any(|fence| content.starts_with(fence))
}

/// Whether a Markdown renderer will read this line as a fence delimiter.
///
/// The argument is a **whole line** of a finished gloss, and its leading
/// whitespace is trimmed here - the opposite contract from
/// [`opens_or_closes_a_fence`], which must be handed a line whose markers and
/// indentation have already come off. The shapes differ because the callers
/// do: this one runs over a gloss, where the indentation inside a fence is the
/// code's own and is kept on purpose (Issue #55).
///
/// That difference is a hazard by itself, so it is stated rather than implied:
/// `"  ```"` is a fence to this function and not to the other, and a caller
/// that reached for the wrong one would lose the fence with nothing to show
/// for it. In the third-party corpus 15 of 2144 fence openers are written at
/// column 2 (`docs/model-runtime-notes.md` §15.6).
///
/// The answers differ too, and neither is the stale copy:
///
/// - [`opens_or_closes_a_fence`] says what **CodeGloss** copies through
///   verbatim, and is narrower than CommonMark on purpose. In a comment a run
///   of tildes is a rule far more often than a fence, and reading one as a
///   fence swallows the prose after it (Issue #56, §15).
/// - This one says what the **editor's Markdown** will make of the gloss once
///   it is rendered. That is not CodeGloss's to narrow: the renderer follows
///   CommonMark, where `~~~` opens a fence whatever a comment meant by it.
///
/// A line, not a document: CommonMark closes a fence only with the character
/// that opened it, and one line cannot know which that was. The caller carries
/// that - see `with_hard_breaks` in `codegloss-lsp`.
pub fn opens_or_closes_a_rendered_fence(line: &str) -> bool {
    let content = line.trim_start();
    RENDERED_FENCES
        .iter()
        .any(|fence| content.starts_with(fence))
}

/// One line's worth of the rebuilt gloss.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// An empty line: a paragraph break.
    Blank,
    /// A line that is copied through as it stands - a fence, the code inside
    /// one, a tag with no prose after it, or an indented example in a language
    /// that marks one that way.
    Verbatim(String),
    /// Prose to translate, and the text emitted in front of it (`@return `,
    /// `- `, `# `).
    Unit { lead: String, text: String },
}

/// A comment block taken apart into the pieces a gloss is rebuilt from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentShape {
    pieces: Vec<Piece>,
}

impl CommentShape {
    /// Reads the structure off a comment exactly as it appears in the file
    /// (that is, off [`CommentBlock::raw`](crate::CommentBlock::raw)).
    pub fn parse(raw: &str, rules: CommentRules) -> Self {
        let block = raw.trim_start().starts_with("/*");
        // Whether a line is an example is a property of the run it stands in
        // and not of the line, so the runs are found before the walk. Empty
        // under rules that mark an example with a fence, and read with `get`,
        // which makes that one branch below rather than two code paths.
        let examples = match rules {
            CommentRules::Fenced => Vec::new(),
            CommentRules::Indented => indented_examples(raw, block),
        };
        let mut pieces = Vec::new();
        let mut paragraph: Option<String> = None;
        let mut fenced = false;

        for (index, line) in raw.lines().enumerate() {
            let content = strip_markers(line, block, index == 0);

            let fence_line = opens_or_closes_a_fence(content);
            if fenced || fence_line {
                if fence_line {
                    fenced = !fenced;
                }
                flush(&mut paragraph, &mut pieces);
                // Copied through with its indentation: inside a fence the
                // leading whitespace is the code, not the comment's syntax.
                pieces.push(Piece::Verbatim(
                    after_markers(line, block, index == 0).to_owned(),
                ));
                continue;
            }

            if let Some(example) = examples.get(index).and_then(Option::as_ref) {
                flush(&mut paragraph, &mut pieces);
                // With its indentation: that is the shape of the example, and
                // under these rules it is the only thing that said so.
                pieces.push(Piece::Verbatim(example.clone()));
                continue;
            }

            if content.is_empty() {
                flush(&mut paragraph, &mut pieces);
                pieces.push(Piece::Blank);
                continue;
            }

            if let Some((lead, prose)) = lead_of(content) {
                flush(&mut paragraph, &mut pieces);
                pieces.push(if prose.is_empty() {
                    Piece::Verbatim(lead.trim_end().to_owned())
                } else {
                    Piece::Unit {
                        lead,
                        text: prose.to_owned(),
                    }
                });
                continue;
            }

            // Plain prose. It joins the lines above it: those were written as
            // one paragraph and read as one sentence.
            match &mut paragraph {
                Some(open) => {
                    open.push(' ');
                    open.push_str(content);
                }
                None => paragraph = Some(content.to_owned()),
            }
        }
        flush(&mut paragraph, &mut pieces);

        Self { pieces }
    }

    /// The prose to translate, in order.
    pub fn units(&self) -> Vec<&str> {
        self.pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Unit { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Rebuilds the block around `translations`, one per unit of
    /// [`Self::units`].
    ///
    /// A unit with no translation keeps its English, which is what makes
    /// [`Self::source`] the same code path as a finished gloss.
    pub fn rebuild(&self, translations: &[String]) -> String {
        let mut rendered: Vec<String> = Vec::with_capacity(self.pieces.len());
        let mut unit = 0;

        for piece in &self.pieces {
            match piece {
                Piece::Blank => rendered.push(String::new()),
                Piece::Verbatim(text) => rendered.push(text.clone()),
                Piece::Unit { lead, text } => {
                    let translated = translations.get(unit).map_or(text.as_str(), String::as_str);
                    unit += 1;
                    rendered.push(format!("{lead}{translated}"));
                }
            }
        }

        // The `/**` and ` */` lines carry no prose, so they leave a blank line
        // at each end that no reader asked for.
        let mut lines = rendered.as_slice();
        while lines.first().is_some_and(String::is_empty) {
            lines = &lines[1..];
        }
        while lines.last().is_some_and(String::is_empty) {
            lines = &lines[..lines.len() - 1];
        }
        lines.join("\n")
    }

    /// The block's own prose in its own structure: what a gloss would look like
    /// if the engine returned its input.
    ///
    /// This is the fallback whenever a translation cannot be trusted, and it is
    /// what the round-trip tests compare against.
    pub fn source(&self) -> String {
        self.rebuild(&[])
    }
}

/// A comment block prepared for translation: its structure, and each of its
/// units with the spans that must not be translated masked out.
///
/// This is the whole of pre-processing and post-processing as the pipeline sees
/// it. [`Self::segments`] is what goes to the engine and [`Self::restore`] is
/// what comes back out of it; nothing in between knows about placeholders,
/// tags or Javadoc stars.
#[derive(Debug, Clone)]
pub struct GlossPlan {
    shape: CommentShape,
    units: Vec<Unit>,
}

/// One unit of the block: its masking table, and the sentences of its masked
/// prose.
///
/// The engine is asked for one sentence at a time even though the masking and
/// the caching stay per unit. FuguMT is a sentence-level model and drops
/// clauses out of a paragraph (see [`split_sentences`]), but a placeholder is
/// only meaningful against the table that produced it, so the table cannot be
/// cut up with the text.
///
/// What the engine sees and what a fragment falls back to are deliberately not
/// the same string. A sentence cut off at a comma is sent with the comma turned
/// into a full stop, because a trailing comma tells the model the sentence is
/// unfinished and it answers with an unfinished Japanese fragment
/// ([`engine_form`] carries the measurement). What is kept here is the piece as
/// it was written, so that the English a lost fragment falls back to is the
/// prose it was cut from, byte for byte. [`Self::sentences`] is therefore the
/// restore side, and [`GlossPlan::segments`] the engine side, of one list.
#[derive(Debug, Clone)]
struct Unit {
    masked: Masked,
    sentences: Vec<String>,
}

impl GlossPlan {
    /// Prepares the comment written as `raw`.
    pub fn new(raw: &str, rules: CommentRules) -> Self {
        let shape = CommentShape::parse(raw, rules);
        let units = shape
            .units()
            .into_iter()
            .map(|text| {
                let masked = mask(text);
                let sentences = split_sentences(masked.masked())
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                Unit { masked, sentences }
            })
            .collect();
        Self { shape, units }
    }

    /// What the engine is asked to translate: one masked segment per sentence,
    /// units in order.
    ///
    /// Every sentence goes through [`engine_form`], which is the one place a
    /// segment is allowed to differ from the piece [`Self::restore`] pairs it
    /// with. The count and the order are the same either way.
    pub fn segments(&self) -> Vec<Segment> {
        self.units
            .iter()
            .flat_map(|unit| unit.sentences.iter().map(String::as_str))
            .map(|sentence| Segment::new(engine_form(sentence)))
            .collect()
    }

    /// Whether there is anything to translate at all.
    pub fn is_empty(&self) -> bool {
        self.units.iter().all(|unit| unit.sentences.is_empty())
    }

    /// The English prose in the block's own structure.
    pub fn source(&self) -> String {
        self.shape.source()
    }

    /// Puts the translations back: sentences first, then placeholders, then
    /// the structure.
    ///
    /// `translations` has one entry per [`Self::segments`], in the same order.
    /// A batch of the wrong length is a broken engine rather than a bad
    /// translation, so the block falls back to its English wholesale; a single
    /// unit that lost a placeholder falls back on its own, in
    /// [`Masked::unmask`].
    ///
    /// A sentence that lost a placeholder falls back to its own English, not
    /// to its neighbours': a paragraph is glossed one sentence at a time, and
    /// one bad sentence taking three good ones back to English costs the
    /// reader more than the mixed line does.
    pub fn restore(&self, translations: &[String]) -> String {
        if translations.len() != self.segments().len() {
            return self.shape.source();
        }

        let mut rest = translations;
        let restored: Vec<String> = self
            .units
            .iter()
            .map(|unit| {
                let (mine, tail) = rest.split_at(unit.sentences.len());
                rest = tail;
                let sentences: Vec<String> = unit
                    .sentences
                    .iter()
                    .zip(mine)
                    .map(|(sentence, translated)| unit.masked.unmask_fragment(sentence, translated))
                    .collect();
                join_sentences(&sentences)
            })
            .collect();
        self.shape.rebuild(&restored)
    }

    /// The English of each segment, in the order of [`Self::segments`].
    ///
    /// What a segment falls back to when its translation loses a placeholder,
    /// and what a translation of the masked form has to be judged against.
    pub fn sources(&self) -> Vec<String> {
        self.units
            .iter()
            .flat_map(|unit| {
                unit.sentences
                    .iter()
                    .map(|sentence| unit.masked.unmask_fragment(sentence, sentence))
            })
            .collect()
    }
}

/// Closes the paragraph being accumulated, if any.
fn flush(paragraph: &mut Option<String>, pieces: &mut Vec<Piece>) {
    if let Some(text) = paragraph.take() {
        pieces.push(Piece::Unit {
            lead: String::new(),
            text,
        });
    }
}

/// Strips the comment syntax off one line: the opener, the closer, the leading
/// `*` of a Javadoc continuation line, the `//` of a line comment.
///
/// The indentation goes with it. That is right for prose - a paragraph is
/// glossed as one line whatever column its continuation lines were typed in -
/// and it is what every reader of this function wants except the one that
/// copies a line of code through, which calls [`after_markers`] instead.
fn strip_markers(line: &str, block: bool, first: bool) -> &str {
    after_markers(line, block, first).trim_start()
}

/// One line with its comment syntax gone but its own indentation kept.
///
/// The single space after a marker belongs to the marker: nobody writing
/// `/// Loads a user.` means the sentence to start with a space. Everything
/// past that space is what the writer typed, and inside a fence that is the
/// shape of the code - the nesting of a doctest is carried by nothing else.
/// So exactly one space is taken off, never a run of them, and never a tab:
/// `///     y();` keeps four spaces, `/// y();` and `///y();` keep none.
///
/// Trailing whitespace is dropped at both ends of that rule. It is invisible
/// in the file and would otherwise reach the gloss.
///
/// One shape this cannot help: a block comment whose continuation lines carry
/// no `*` (`/*! ... */` written flush against column zero, as the `regex`
/// crates write their module docs). There the leading whitespace of a line is
/// the file's indentation and the writer's indentation at the same time, and
/// one line on its own does not say which. Those lines keep coming out flush.
fn after_markers(line: &str, block: bool, first: bool) -> &str {
    let mut content = line.trim();

    if block {
        if first && let Some(opener) = BLOCK_OPENERS.iter().find(|o| content.starts_with(**o)) {
            content = &content[opener.len()..];
        }
        if let Some(rest) = content.strip_suffix(BLOCK_CLOSER) {
            content = rest;
        }
        if !first && let Some(rest) = content.trim_start().strip_prefix('*') {
            content = rest;
        }
    } else if let Some(marker) = LINE_MARKERS.iter().find(|m| content.starts_with(**m)) {
        content = &content[marker.len()..];
    }

    content.strip_prefix(' ').unwrap_or(content).trim_end()
}

/// One line with its comment syntax gone and its indentation exactly as
/// written, the undecorated continuation lines of a block comment included.
///
/// The question [`after_markers`] answers, asked by a caller that is allowed a
/// different answer - so a second function and not a flag, the way
/// [`opens_or_closes_a_fence`] and [`opens_or_closes_a_rendered_fence`] are two
/// functions. [`after_markers`] trims a line before it does anything else,
/// which throws away the leading whitespace of a `/* ... */` continuation line
/// that carries no `*`. It does that deliberately: there, that whitespace is
/// the file's indentation and the writer's at the same time, and one line on
/// its own cannot say which.
///
/// Under [`CommentRules::Indented`] nothing else says it either, so the
/// indentation is kept and the ambiguity is paid for instead - by
/// [`indented_examples`], which takes the whole block's common indentation off
/// before it reads anything.
fn after_markers_as_written(line: &str, block: bool, first: bool) -> &str {
    if !block {
        // A line comment's marker ends at a fixed column, so what follows it is
        // the writer's alone and `after_markers` already keeps it. Measured
        // over 858,710 comment lines the two never disagree here
        // (`docs/model-runtime-notes.md` §16).
        return after_markers(line, block, first);
    }

    let mut content = line.trim_end();
    if first {
        content = content.trim_start();
        if let Some(opener) = BLOCK_OPENERS.iter().find(|o| content.starts_with(**o)) {
            content = &content[opener.len()..];
        }
    }
    // IMPORTANT: the closer comes off before the star, in the order
    // [`after_markers`] takes them off. The other way round reads the `*` of a
    // line that is only `*/` as the decoration and leaves a `/` behind - a
    // non-blank line at column 0 that no writer typed, which ends an example
    // and empties the common indentation of every block that has one.
    if let Some(rest) = content.strip_suffix(BLOCK_CLOSER) {
        content = rest;
    }
    if !first && let Some(rest) = content.trim_start().strip_prefix('*') {
        // A decorated continuation line: the `*` is the marker, and what
        // follows it is read exactly as [`after_markers`] reads it.
        content = rest;
    }
    content.strip_prefix(' ').unwrap_or(content).trim_end()
}

/// The lines of `raw` that belong to an indented example, each with the
/// indentation that says so, and `None` for every other line.
///
/// `go/doc/comment`'s span rule (`parse.go`, `parseSpans`), with its `unindent`
/// step narrowed and its `forceIndent` fix-ups left out: blank lines are
/// skipped, a line beginning with a space or a tab opens a span, the span runs
/// to the line before the next one that is neither blank nor indented, trailing
/// blank lines are dropped, and one following line is taken in if it begins
/// with `}`. A span whose first line carries a list marker is a list, not an
/// example.
///
/// `unindent` is Go's first step and is kept here only for a `/* ... */`
/// comment, where the leading whitespace of a continuation line is the file's
/// indentation and the writer's at once and only the block as a whole tells
/// them apart. It is dropped for a run of `//` lines, where applying it would
/// be actively wrong: CodeGloss cuts a doc comment into one block per
/// paragraph, so a gofmt'ed example is a block of its own whose common prefix
/// is the very tab that says it is an example. Measured over `GOROOT`,
/// unindenting everything reaches 58.2% of Go's own code lines and unindenting
/// only block comments 91.6% (`docs/model-runtime-notes.md` §16).
///
/// `forceIndent` is Go's rescue for code that was pasted in without being
/// indented. Go says itself that it can never fire on a gofmt'ed comment, and
/// it costs prose, so it is left out; the 385 lines that costs are counted in
/// §16.
fn indented_examples(raw: &str, block: bool) -> Vec<Option<String>> {
    let mut lines: Vec<&str> = raw
        .lines()
        .enumerate()
        .map(|(index, line)| after_markers_as_written(line, block, index == 0))
        .collect();
    if block {
        let common = common_indentation(&lines);
        for line in &mut lines {
            *line = line.strip_prefix(common).unwrap_or(line);
        }
    }

    let indented = |line: &str| line.starts_with([' ', '\t']);
    let blank = |line: &str| line.trim().is_empty();
    let mut example = vec![None; lines.len()];
    let mut index = 0;

    while index < lines.len() {
        while index < lines.len() && blank(lines[index]) {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }

        let start = index;
        if !indented(lines[index]) {
            // Prose. It ends at the next blank or indented line, and the line
            // that ends it is looked at again as the start of the next span.
            index += 1;
            while index < lines.len() && !blank(lines[index]) && !indented(lines[index]) {
                index += 1;
            }
            continue;
        }

        index += 1;
        while index < lines.len() && (blank(lines[index]) || indented(lines[index])) {
            index += 1;
        }
        let mut end = index;
        while end > start && blank(lines[end - 1]) {
            end -= 1;
        }
        // Somebody pasted a function in and forgot to indent its closing brace.
        // Go takes that line too, and says why: a gofmt'ed comment can never
        // reach here, because a gofmt'ed example is followed by a blank line or
        // by the end of the comment.
        if end < lines.len() && lines[end].starts_with('}') {
            end += 1;
        }
        if !opens_a_list(lines[start]) {
            for (slot, line) in example.iter_mut().zip(&lines).take(end).skip(start) {
                *slot = Some((*line).to_owned());
            }
        }
        index = end;
    }

    example
}

/// The run of spaces and tabs that every non-blank line of `lines` begins with.
///
/// Borrowed from the first non-blank line, so what comes back is always a whole
/// number of characters of that line. Spaces and tabs are one byte each, which
/// is what makes counting the shared prefix in bytes both correct and cheap.
fn common_indentation<'a>(lines: &[&'a str]) -> &'a str {
    let mut common: Option<&'a str> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let end = line
            .char_indices()
            .find(|(_, character)| *character != ' ' && *character != '\t')
            .map_or(line.len(), |(offset, _)| offset);
        let indentation = &line[..end];
        common = Some(match common {
            None => indentation,
            Some(common) => {
                let shared = common
                    .bytes()
                    .zip(indentation.bytes())
                    .take_while(|(one, other)| one == other)
                    .count();
                &common[..shared]
            }
        });
    }
    common.unwrap_or_default()
}

/// `go/doc/comment`'s `listMarker`: a bullet or a number, then a space or a
/// tab, then something.
///
/// A list is written indented and is still a list, so without this every
/// bulleted paragraph in a Go comment would be copied through untranslated.
/// Measured over `GOROOT` it is the difference between 1,208 lines wrongly
/// copied and 4,067 (`docs/model-runtime-notes.md` §16).
fn opens_a_list(line: &str) -> bool {
    let line = line.trim();
    let Some(marker) = line.chars().next() else {
        return false;
    };

    let rest = if matches!(marker, '\u{2022}' | '*' | '+' | '-') {
        &line[marker.len_utf8()..]
    } else if marker.is_ascii_digit() {
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let after = &line[digits..];
        match after.strip_prefix('.').or_else(|| after.strip_prefix(')')) {
            Some(rest) => rest,
            None => return false,
        }
    } else {
        return false;
    };

    rest.starts_with([' ', '\t']) && !rest.trim().is_empty()
}

/// The part of a line that is emitted verbatim in front of its translation:
/// a doc tag, a list bullet or a Markdown heading.
fn lead_of(content: &str) -> Option<(String, &str)> {
    tag_lead(content).or_else(|| marker_lead(content))
}

/// `@return `, `@param name `, `@throws Type `.
fn tag_lead(content: &str) -> Option<(String, &str)> {
    let letters = content
        .strip_prefix('@')?
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    if letters == 0 {
        return None;
    }

    let (tag, rest) = content.split_at('@'.len_utf8() + letters);
    let rest = rest.trim_start();

    // `@param id the user id`: `id` names an argument, so it is an identifier
    // whatever it looks like, and the engine never needs to see it.
    if TAGS_WITH_A_NAME.contains(&tag) && !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (name, tail) = rest.split_at(end);
        return Some((format!("{tag} {name} "), tail.trim_start()));
    }
    Some((format!("{tag} "), rest))
}

/// `- `, `+ `, `1. `, `# ` - the Markdown decoration of a line.
fn marker_lead(content: &str) -> Option<(String, &str)> {
    let mut end = 0;
    let mut characters = content.chars();

    match characters.next()? {
        '-' | '+' => end += 1,
        '#' => {
            end += 1;
            end += characters
                .clone()
                .take_while(|character| *character == '#')
                .count();
        }
        digit if digit.is_ascii_digit() => {
            end += 1;
            end += content[end..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            if !matches!(content[end..].chars().next(), Some('.' | ')')) {
                return None;
            }
            end += 1;
        }
        _ => return None,
    }

    // The space is what tells `- item` from `-1` and `#tag`.
    let rest = content[end..].strip_prefix(' ')?.trim_start();
    Some((format!("{} ", &content[..end]), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example Issue #1 gives for a block that has to survive translation.
    const JAVADOC: &str = concat!(
        "/**\n",
        " * Returns the currently authenticated user.\n",
        " *\n",
        " * @return authenticated user\n",
        " * @throws AuthenticationException if authentication failed\n",
        " */",
    );

    #[test]
    fn a_javadoc_block_becomes_one_unit_per_paragraph_and_per_tag() {
        assert_eq!(
            CommentShape::parse(JAVADOC, CommentRules::Fenced).units(),
            [
                "Returns the currently authenticated user.",
                "authenticated user",
                "if authentication failed",
            ]
        );
    }

    /// The markers are gone, the structure is not: the blank line and the two
    /// tag lines are still lines of their own.
    #[test]
    fn the_structure_of_a_javadoc_block_survives_the_round_trip() {
        assert_eq!(
            CommentShape::parse(JAVADOC, CommentRules::Fenced).source(),
            concat!(
                "Returns the currently authenticated user.\n",
                "\n",
                "@return authenticated user\n",
                "@throws AuthenticationException if authentication failed",
            )
        );
    }

    #[test]
    fn a_translation_is_poured_back_into_the_same_shape() {
        let shape = CommentShape::parse(JAVADOC, CommentRules::Fenced);
        let gloss = shape.rebuild(&[
            "現在認証されているユーザーを返します。".to_owned(),
            "認証済みのユーザー".to_owned(),
            "認証に失敗した場合".to_owned(),
        ]);

        assert_eq!(
            gloss,
            concat!(
                "現在認証されているユーザーを返します。\n",
                "\n",
                "@return 認証済みのユーザー\n",
                "@throws AuthenticationException 認証に失敗した場合",
            )
        );
    }

    /// A paragraph reaches the engine one sentence at a time, and comes back as
    /// the single line it was written as.
    #[test]
    fn a_unit_is_asked_for_one_sentence_at_a_time() {
        let plan = GlossPlan::new(
            "/// Returns the user. Fails when `id` is unknown. Nothing is cached.",
            CommentRules::Fenced,
        );
        assert_eq!(
            plan.segments()
                .iter()
                .map(|segment| segment.text().to_owned())
                .collect::<Vec<_>>(),
            [
                "Returns the user.",
                "Fails when X0Q is unknown.",
                "Nothing is cached.",
            ]
        );
        assert_eq!(
            plan.restore(&[
                "ユーザを返します。".to_owned(),
                "X0Q が不明な場合は失敗します。".to_owned(),
                "何もキャッシュされません。".to_owned(),
            ]),
            "ユーザを返します。`id` が不明な場合は失敗します。何もキャッシュされません。"
        );
    }

    /// A sentence that dropped a placeholder takes only itself back to English:
    /// the sentence beside it was fine and stays glossed.
    #[test]
    fn only_the_sentence_that_lost_a_placeholder_falls_back() {
        let plan = GlossPlan::new(
            "/// Returns the user. Fails when `id` is unknown.",
            CommentRules::Fenced,
        );
        assert_eq!(
            plan.sources(),
            ["Returns the user.", "Fails when `id` is unknown."]
        );
        assert_eq!(
            plan.restore(&[
                "ユーザを返します。".to_owned(),
                "不明な場合は失敗します。".to_owned(),
            ]),
            "ユーザを返します。Fails when `id` is unknown."
        );
    }

    /// A batch of the wrong length is a broken engine: the count to match is
    /// the number of sentences now, not the number of units.
    #[test]
    fn a_batch_of_the_wrong_length_falls_back() {
        let plan = GlossPlan::new(
            "/// Returns the user. Nothing is cached.",
            CommentRules::Fenced,
        );
        assert_eq!(plan.segments().len(), 2);
        assert_eq!(
            plan.restore(&["ユーザを返します。".to_owned()]),
            "Returns the user. Nothing is cached."
        );
    }

    /// The parser merges a run of `//` lines into one sentence, and that holds
    /// here: the engine must see the sentence, not three fragments.
    #[test]
    fn consecutive_prose_lines_are_one_unit() {
        let shape = CommentShape::parse(
            "// Return the cached user\n// if there is one.",
            CommentRules::Fenced,
        );
        assert_eq!(shape.units(), ["Return the cached user if there is one."]);
        assert_eq!(shape.source(), "Return the cached user if there is one.");
    }

    #[test]
    fn every_comment_marker_is_stripped() {
        for raw in [
            "// Note.",
            "/// Note.",
            "//! Note.",
            "/* Note. */",
            "/** Note. */",
            "/*! Note. */",
        ] {
            assert_eq!(
                CommentShape::parse(raw, CommentRules::Fenced).units(),
                ["Note."],
                "in {raw:?}"
            );
        }
    }

    #[test]
    fn a_blank_line_separates_two_paragraphs() {
        let shape = CommentShape::parse("/// One.\n///\n/// Two.", CommentRules::Fenced);
        assert_eq!(shape.units(), ["One.", "Two."]);
        assert_eq!(shape.source(), "One.\n\nTwo.");
    }

    #[test]
    fn a_named_tag_keeps_its_argument_out_of_the_translation() {
        let shape = CommentShape::parse("/// @param id the user to load", CommentRules::Fenced);
        assert_eq!(shape.units(), ["the user to load"]);
        assert_eq!(shape.source(), "@param id the user to load");
    }

    #[test]
    fn a_tag_without_prose_is_copied_through() {
        let shape = CommentShape::parse("/// @deprecated", CommentRules::Fenced);
        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "@deprecated");
    }

    #[test]
    fn a_list_item_and_a_heading_keep_their_markers() {
        let shape = CommentShape::parse(
            "/// # Panics\n///\n/// - Fails on a miss.\n/// 1. Then this.",
            CommentRules::Fenced,
        );
        assert_eq!(shape.units(), ["Panics", "Fails on a miss.", "Then this."]);
        assert_eq!(
            shape.source(),
            "# Panics\n\n- Fails on a miss.\n1. Then this."
        );
    }

    /// A fenced example is code. Translating it would break it, so the whole
    /// fence is copied through instead.
    #[test]
    fn a_fenced_code_block_is_never_translated() {
        let shape = CommentShape::parse(
            concat!(
                "/// Loads a user.\n",
                "///\n",
                "/// ```\n",
                "/// let user = find_user(id);\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert_eq!(shape.units(), ["Loads a user."]);
        assert_eq!(
            shape.source(),
            "Loads a user.\n\n```\nlet user = find_user(id);\n```"
        );
    }

    /// The core-side statement of what `codegloss-parser` now delivers: a
    /// doctest with a blank line in it arrives in one piece, and every line of
    /// it - the blank one included - is code.
    #[test]
    fn a_fenced_example_with_a_blank_line_in_it_is_still_verbatim() {
        let shape = CommentShape::parse(
            concat!(
                "/// ```\n",
                "/// a = 1;\n",
                "///\n",
                "/// b = 2;\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "```\na = 1;\n\nb = 2;\n```");
    }

    /// Issue #55: the nesting of an example is the example. A doctest whose
    /// body came back flush against the left margin is the code the reader is
    /// looking at, rewritten.
    #[test]
    fn the_indentation_inside_a_fence_is_kept() {
        let shape = CommentShape::parse(
            concat!(
                "/// ```\n",
                "/// if let Some(user) = find_user(id) {\n",
                "///     println!(\"{user}\");\n",
                "///     log(user);\n",
                "/// }\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert!(shape.units().is_empty());
        assert_eq!(
            shape.source(),
            concat!(
                "```\n",
                "if let Some(user) = find_user(id) {\n",
                "    println!(\"{user}\");\n",
                "    log(user);\n",
                "}\n",
                "```",
            )
        );
    }

    /// Where the marker ends and the code begins: one space, no more and no
    /// less. Nobody writing `/// let x = 1;` means the line to start with a
    /// space, and nobody writing `///     y();` means it to start flush.
    #[test]
    fn the_space_after_a_marker_is_not_indentation() {
        let shape = CommentShape::parse(
            concat!(
                "/// ```\n",
                "///     four();\n",
                "/// one();\n",
                "///none();\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert_eq!(shape.source(), "```\n    four();\none();\nnone();\n```");
    }

    /// The same rule under a Javadoc star. Neither corpus behind
    /// `docs/model-runtime-notes.md` §14 holds a starred block comment with a
    /// fence in it, so this is the only place the shape is pinned at all.
    #[test]
    fn a_starred_block_comment_keeps_the_indentation_inside_its_fence() {
        let spaced = CommentShape::parse(
            "/**\n * ```\n *     y();\n * ```\n */",
            CommentRules::Fenced,
        );
        assert_eq!(spaced.source(), "```\n    y();\n```");

        // The space after the star is optional, and its absence is not
        // indentation either: what the star sheds is the star.
        let tight =
            CommentShape::parse("/**\n *```\n *     y();\n *```\n */", CommentRules::Fenced);
        assert_eq!(tight.source(), "```\n    y();\n```");
    }

    /// A tab is one character of indentation and cannot be counted as spaces.
    /// `regex-syntax` writes its `hir` doctests this way.
    #[test]
    fn a_tab_inside_a_fence_is_content() {
        let shape = CommentShape::parse(
            concat!(
                "/// ```\n",
                "///\tif x {\n",
                "///\t\ty();\n",
                "/// }\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert_eq!(shape.source(), "```\n\tif x {\n\t\ty();\n}\n```");
    }

    /// Every byte of a fenced line is copied, so every byte of one has to be
    /// found on a character boundary.
    ///
    /// `GlossPlan::new` runs inside the worker's async task, where one panic
    /// takes the session's translations with it (commit 3ea8a36 was exactly
    /// that, one module over). A rule that indexed past the marker by hand
    /// would panic on the third line here.
    #[test]
    fn a_fence_line_survives_crlf_and_multibyte_input() {
        assert_eq!(
            CommentShape::parse(
                "/// ```\r\n///     let x = 1;\r\n/// ```",
                CommentRules::Fenced
            )
            .source(),
            "```\n    let x = 1;\n```"
        );
        assert_eq!(
            CommentShape::parse("/// ```\n///     日本語\n/// ```", CommentRules::Fenced).source(),
            "```\n    日本語\n```"
        );
        assert_eq!(
            CommentShape::parse("/// ```\n///\u{3000}全角\n/// ```", CommentRules::Fenced).source(),
            "```\n\u{3000}全角\n```"
        );
    }

    /// The other side of the rule: outside a fence the indentation is not
    /// content, and a paragraph is glossed as one line however its
    /// continuation lines were laid out.
    #[test]
    fn prose_lines_are_still_trimmed() {
        let shape = CommentShape::parse(
            "///   Prose   \n///     wrapped onto two lines.",
            CommentRules::Fenced,
        );
        assert_eq!(shape.units(), ["Prose wrapped onto two lines."]);
        assert_eq!(shape.source(), "Prose wrapped onto two lines.");
    }

    /// The shape this rule cannot help, stated so that it is a decision rather
    /// than a surprise: in a block comment whose continuation lines carry no
    /// `*`, the leading whitespace is the file's indentation and the writer's
    /// indentation at once. The `regex` crates write their module docs this
    /// way (8 blocks of the third-party corpus in §14.8), and they keep coming
    /// out flush - the same as before Issue #55, not worse.
    #[test]
    fn a_block_comment_without_stars_cannot_keep_its_indentation() {
        let shape = CommentShape::parse(
            "/*!\n```\nif x {\n    y();\n}\n```\n*/",
            CommentRules::Fenced,
        );
        assert_eq!(shape.source(), "```\nif x {\ny();\n}\n```");
    }

    /// The boundary the parser depends on. It decides where a block ends by
    /// this answer, so widening it to a setext underline would start merging
    /// the paragraphs a `-----` was written to separate.
    ///
    /// A run of tildes is on the false side, which CommonMark is not (Issue
    /// #56): in a comment it is a rule far more often than a fence, and the
    /// line cannot say which. It sits here beside `-----` and `//////////`
    /// because that is what it is.
    #[test]
    fn only_a_backtick_fence_opens_a_fence() {
        for content in ["```", "```rust", "```text"] {
            assert!(opens_or_closes_a_fence(content), "{content:?}");
        }
        for content in [
            "",
            "//////////",
            "====",
            "-----",
            "# Heading",
            "``inline``",
            "~~~",
            "~~~~~~~~~~",
            "~~~~~ Section ~~~~~",
            "~~~~~~Path",
        ] {
            assert!(!opens_or_closes_a_fence(content), "{content:?}");
        }
    }

    /// The two fence predicates pinned against each other, which is the only
    /// place a reader can see at once that the second is not a copy of the
    /// first gone stale.
    ///
    /// They differ twice over. The argument: `opens_or_closes_a_fence` is
    /// handed a line already stripped and trimmed, and the rendered one is
    /// handed the whole line and trims it itself, because a gloss keeps the
    /// indentation inside a fence (Issue #55). And the answer: a run of tildes
    /// is not a fence CodeGloss copies through (Issue #56) but is one
    /// CommonMark opens, and what the editor's Markdown does with a gloss is
    /// not ours to narrow.
    #[test]
    fn the_two_fence_predicates_answer_different_questions() {
        // An indented fence: a whole line to one, and not the shape the other
        // takes at all. Reaching for the wrong one here loses the fence.
        assert!(!opens_or_closes_a_fence("  ```"));
        assert!(opens_or_closes_a_rendered_fence("  ```"));

        // A tilde fence: CommonMark's, not CodeGloss's.
        assert!(!opens_or_closes_a_fence("~~~"));
        assert!(opens_or_closes_a_rendered_fence("~~~"));

        // Where they agree, which is every fence written at column 0.
        assert!(opens_or_closes_a_fence("```"));
        assert!(opens_or_closes_a_rendered_fence("```"));
        assert!(!opens_or_closes_a_fence("Returns the user."));
        assert!(!opens_or_closes_a_rendered_fence("Returns the user."));
    }

    /// A rule is not the opener of an example that never closes, so the prose
    /// after it is still prose: both of these used to produce no translation
    /// unit at all and reach the reader in English (Issue #56).
    ///
    /// What a rule becomes instead is a word in the paragraph it interrupts -
    /// the price of this change, and the reason the parser drops a word-less
    /// one before `CommentShape` ever sees it
    /// (`a_tilde_rule_breaks_a_run_like_any_other_decoration`). A decorated
    /// one carries a word, so it stays, exactly as `// ==== Section ====`
    /// always has.
    #[test]
    fn a_tilde_rule_does_not_swallow_the_prose_after_it() {
        for (raw, unit) in [
            (
                "/// ~~~~~ Section ~~~~~\n/// Prose after a decorated banner.",
                "~~~~~ Section ~~~~~ Prose after a decorated banner.",
            ),
            (
                "/// ~~~~~~~~~~\n/// Prose after a bare tilde rule.",
                "~~~~~~~~~~ Prose after a bare tilde rule.",
            ),
        ] {
            assert_eq!(
                CommentShape::parse(raw, CommentRules::Fenced).units(),
                [unit],
                "in {raw:?}"
            );
        }
    }

    /// The other half, and the one with a live case behind it: `syn` draws its
    /// attribute diagram with tildes inside a ```` ```text ```` fence. Reading
    /// those as a closing fence cuts the diagram in two and hands the second
    /// half to the engine as prose - which is where the caret row loses a
    /// caret and stops lining up with what it points at
    /// (`docs/model-runtime-notes.md` §15.1).
    #[test]
    fn a_tilde_line_inside_a_fence_does_not_close_it() {
        let shape = CommentShape::parse(
            concat!(
                "/// ```text\n",
                "/// #[derive(Copy, Clone)]\n",
                "///   ~~~~~~Path\n",
                "///   ^^^^^^^^^^^^^^^^^^^Meta::List\n",
                "/// ```",
            ),
            CommentRules::Fenced,
        );

        assert!(shape.units().is_empty(), "{shape:?}");
        assert_eq!(
            shape.source(),
            concat!(
                "```text\n",
                "#[derive(Copy, Clone)]\n",
                "  ~~~~~~Path\n",
                "  ^^^^^^^^^^^^^^^^^^^Meta::List\n",
                "```",
            )
        );
    }

    #[test]
    fn a_block_comment_without_stars_keeps_its_prose() {
        let shape = CommentShape::parse(
            "/* Loads the user\n   from the cache. */",
            CommentRules::Fenced,
        );
        assert_eq!(shape.units(), ["Loads the user from the cache."]);
    }

    #[test]
    fn a_comment_with_no_prose_has_no_units() {
        let shape = CommentShape::parse("//", CommentRules::Fenced);
        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "");
    }

    #[test]
    fn a_plan_hands_the_engine_masked_segments() {
        let plan = GlossPlan::new(
            "/// Returns `UserDetails` when authentication succeeds.",
            CommentRules::Fenced,
        );
        assert_eq!(
            plan.segments()
                .iter()
                .map(|segment| segment.text().to_owned())
                .collect::<Vec<_>>(),
            ["Returns X0Q when authentication succeeds.".to_owned()]
        );
    }

    /// A sentence cut off at a comma reaches the engine with a full stop in the
    /// comma's place, and falls back to the prose it was cut from.
    ///
    /// The two halves of the one decision this change is: the rewrite is on the
    /// way out and only there. [`GlossPlan::sources`] is what a fragment whose
    /// translation loses a placeholder is shown as, and putting those back
    /// reproduces the comment exactly - the comma is where it was written, and
    /// [`join_sentences`] puts the space after it.
    #[test]
    fn a_comma_split_reaches_the_engine_terminated_and_falls_back_untouched() {
        let plan = GlossPlan::new(
            "/// Dropping it closes the socket and wakes every task blocked on accept, \
             which is why the shutdown is not graceful.",
            CommentRules::Fenced,
        );

        let segments: Vec<String> = plan
            .segments()
            .iter()
            .map(|segment| segment.text().to_owned())
            .collect();
        assert_eq!(
            segments,
            [
                "Dropping it closes the socket and wakes every task blocked on accept.",
                "which is why the shutdown is not graceful.",
            ]
        );

        let sources = plan.sources();
        assert_eq!(
            sources,
            [
                "Dropping it closes the socket and wakes every task blocked on accept,",
                "which is why the shutdown is not graceful.",
            ]
        );
        assert_eq!(plan.restore(&sources), plan.source());
    }

    /// Every shape a comment takes, as a raw comment block.
    ///
    /// The table the two round-trip tests below share. A unit of more than one
    /// sentence is in it several times over, because that is the shape both of
    /// them used to have to avoid: [`join_sentences`] put no space after a full
    /// stop, so an echoed `Returns the user. Nothing is cached.` came back as
    /// `Returns the user.Nothing is cached.` (Issue #49).
    const RAWS: [&str; 11] = [
        JAVADOC,
        "// Return the cached user.",
        "/// Returns `UserDetails` when authentication succeeds.",
        "/// See https://example.com/docs for the protocol.",
        "// TODO: replace find_user with UserRepository::load().",
        "/// Loads a user.\n///\n/// ```\n/// let user = find_user(id);\n/// ```",
        // More than one sentence per unit, which is the shape Issue #49 broke.
        "/// Returns the user. Nothing is cached.",
        "/// Returns `UserDetails`. Fails when `id` is unknown.",
        "// Really?! It does. Wait... The rest arrives later.",
        // A sentence that finishes on the line after the one it opened on: the
        // two `//` lines are one unit, and the boundary is inside it.
        "// Returns the user. Nothing is\n// cached, and nothing is written back.",
        // The same, inside a Javadoc paragraph, with a tag line after it.
        "/**\n * Returns the user. Nothing is cached.\n *\n * @return the user\n */",
    ];

    /// The property the whole phase exists for: with an engine that returns its
    /// input, a block comes back exactly as its prose went in.
    ///
    /// Every raw here is one whose segments are its sentences. A unit split at
    /// a comma is not, and cannot be: what such a segment carries is a full stop
    /// the comment never had ([`engine_form`]), so an engine that echoes it
    /// echoes that too. The exact property for those is stated in
    /// [`restore_of_sources_is_the_source_for_every_shape`], over
    /// [`GlossPlan::sources`] - the English a fragment actually falls back to -
    /// which is the string the rewrite was kept out of.
    ///
    /// An engine that returns its input is not a hypothetical: it is
    /// `PassthroughTranslator`, the engine every install runs until its model
    /// pack arrives, and the one a build without the `candle` feature has at
    /// all. What this test asserts is what such a reader is shown.
    #[test]
    fn a_passthrough_translation_restores_the_source_exactly() {
        for raw in RAWS {
            let plan = GlossPlan::new(raw, CommentRules::Fenced);
            let translations: Vec<String> = plan
                .segments()
                .iter()
                .map(|segment| segment.text().to_owned())
                .collect();

            assert_eq!(plan.restore(&translations), plan.source(), "in {raw:?}");
        }
    }

    /// The same round trip over [`GlossPlan::sources`]: the English a fragment
    /// whose translation lost a placeholder falls back to is the prose it was
    /// cut from, joined back up unchanged.
    ///
    /// Stronger than the test above in the one place that test cannot reach -
    /// a unit cut at a comma, whose segment carries a full stop the comment
    /// never had - and weaker nowhere, so the table is the same one.
    #[test]
    fn restore_of_sources_is_the_source_for_every_shape() {
        for raw in RAWS.into_iter().chain([
            "/// Dropping it closes the socket and wakes every task blocked on accept, \
             which is why the shutdown is not graceful.",
        ]) {
            let plan = GlossPlan::new(raw, CommentRules::Fenced);
            assert_eq!(plan.restore(&plan.sources()), plan.source(), "in {raw:?}");
        }
    }

    /// An engine that answers with the wrong number of translations is a broken
    /// engine; pairing them up anyway would gloss each line with its neighbour.
    #[test]
    fn a_batch_of_the_wrong_length_falls_back_to_the_source() {
        let plan = GlossPlan::new(JAVADOC, CommentRules::Fenced);
        assert_eq!(plan.restore(&["訳".to_owned()]), plan.source());
    }

    #[test]
    fn a_unit_that_lost_a_placeholder_falls_back_on_its_own() {
        let plan = GlossPlan::new(
            "/// Returns `UserDetails`.\n///\n/// @return the user",
            CommentRules::Fenced,
        );
        let gloss = plan.restore(&["返します。".to_owned(), "ユーザー".to_owned()]);

        // The first unit lost `X0Q` and keeps its English; the second is fine.
        assert_eq!(gloss, "Returns `UserDetails`.\n\n@return ユーザー");
    }

    /// The one test that pins both directions of the gate. Ungate the rule and
    /// the `Fenced` half fails, which is every Rust comment ever indented for
    /// looks; delete the branch and the `Indented` half fails, which is Issue
    /// #30 back again.
    #[test]
    fn an_indented_run_is_an_example_only_under_the_rules_that_say_so() {
        let raw = "//\tpattern:\n//\t\t{ term }";

        let fenced = CommentShape::parse(raw, CommentRules::Fenced);
        assert_eq!(fenced.units(), ["pattern: { term }"]);
        assert_eq!(fenced.source(), "pattern: { term }");

        let indented = CommentShape::parse(raw, CommentRules::Indented);
        assert!(indented.units().is_empty(), "{indented:?}");
        assert_eq!(indented.source(), "\tpattern:\n\t\t{ term }");
    }

    /// A span ends at the first line that is neither blank nor indented, and
    /// the prose on either side of it is still prose.
    #[test]
    fn an_example_ends_where_the_indentation_does() {
        let shape = CommentShape::parse(
            "// Example:\n//\n//\tf(x)\n//\n//\tg(y)\n//\n// Both return an error.",
            CommentRules::Indented,
        );

        assert_eq!(shape.units(), ["Example:", "Both return an error."]);
        assert_eq!(
            shape.source(),
            "Example:\n\n\tf(x)\n\n\tg(y)\n\nBoth return an error."
        );
    }

    /// Go takes the unindented `}` that closes a pasted-in function, and so
    /// does this. Without it the brace is glossed as if it were a sentence.
    #[test]
    fn an_unindented_closing_brace_belongs_to_the_example_above_it() {
        let shape = CommentShape::parse(
            "// Example:\n//\n//\tfunc main() {\n//\t\tprintln()\n// }",
            CommentRules::Indented,
        );

        assert_eq!(shape.units(), ["Example:"]);
        assert_eq!(
            shape.source(),
            "Example:\n\n\tfunc main() {\n\t\tprintln()\n}"
        );
    }

    /// A list is written indented and is still a list. Without the guard every
    /// bulleted paragraph in a Go comment is copied through untranslated -
    /// measured, 4,067 lines instead of 1,208
    /// (`docs/model-runtime-notes.md` §16).
    #[test]
    fn a_list_is_not_an_example() {
        let shape = CommentShape::parse(
            "//   - Anything else comes before RC4\n//   - ECDHE comes before anything else",
            CommentRules::Indented,
        );

        assert_eq!(
            shape.units(),
            [
                "Anything else comes before RC4",
                "ECDHE comes before anything else"
            ]
        );
    }

    /// A `/* ... */` written inside a function indents every line of itself,
    /// and that indentation is the file's rather than the writer's. Taking the
    /// block's common prefix off first is what keeps its prose prose; without
    /// it the whole comment reads as one example and its gloss disappears.
    #[test]
    fn a_block_comment_is_unindented_before_the_rule_is_applied() {
        let tabs = CommentShape::parse(
            "/*\n\t\tProse that wraps.\n\t\tSecond line.\n\t*/",
            CommentRules::Indented,
        );
        assert_eq!(tabs.units(), ["Prose that wraps. Second line."]);

        let spaces = CommentShape::parse(
            "/*\n\n   Prose.\n\n   More prose.\n*/",
            CommentRules::Indented,
        );
        assert_eq!(spaces.units(), ["Prose.", "More prose."]);
    }

    /// The same step must not reach a run of `//` lines. CodeGloss cuts a doc
    /// comment into one block per paragraph, so a gofmt'ed example arrives as a
    /// block of its own whose common prefix is the very tab that says it is an
    /// example. Measured over `GOROOT`, unindenting everything reaches 58.2% of
    /// Go's code lines against 91.6% (`docs/model-runtime-notes.md` §16).
    #[test]
    fn a_run_of_line_comments_is_not_unindented() {
        let shape = CommentShape::parse("//\tf(x)\n//\tg(y)", CommentRules::Indented);

        assert!(shape.units().is_empty(), "{shape:?}");
        assert_eq!(shape.source(), "\tf(x)\n\tg(y)");
    }

    /// [`after_markers`] and [`after_markers_as_written`] answer the same
    /// question for everything but one shape, and this is the table of it. The
    /// disagreement is the last row: a continuation line of a block comment
    /// that carries no `*`, where one of them keeps the indentation and the
    /// other does not.
    #[test]
    fn the_two_marker_strippers_agree_except_on_an_undecorated_continuation_line() {
        for (line, block, first) in [
            ("// prose", false, true),
            ("/// prose", false, false),
            ("//\tf(x)", false, false),
            ("/* prose", true, true),
            ("/** prose", true, true),
            (" * prose", true, false),
            (" *\tf(x)", true, false),
            // Only the closer. Taking the star off first would answer "/",
            // a line at column 0 that nobody typed, and every block's common
            // indentation would come out empty.
            (" */", true, false),
            ("   ", true, false),
            ("", true, false),
        ] {
            assert_eq!(
                after_markers_as_written(line, block, first),
                after_markers(line, block, first),
                "in {line:?}"
            );
        }

        assert_eq!(after_markers("   prose", true, false), "prose");
        assert_eq!(after_markers_as_written("   prose", true, false), "  prose");
    }

    /// This rule is entirely about leading whitespace, and it runs inside the
    /// LSP worker's task, where one panic stops every gloss of the session
    /// (`git show 3ea8a36`). Nothing here asserts an answer - the assertion is
    /// that there is one.
    #[test]
    fn an_example_does_not_panic_on_awkward_input() {
        for raw in [
            "",
            "//",
            "/*",
            "*/",
            "/*\n*/",
            "//\t",
            "//\t日本語",
            "//\t\t§ 4",
            "//\u{3000}全角",
            "// - ",
            "//1.\ta",
            "//1.",
            // Indented, so the list marker is looked at - and it is three
            // bytes wide.
            "//\t•\tbullet",
            "//\t•",
            "//\t1.\ta",
            "//\t§",
            "//\t```\r\n//\tx\r\n//\t```",
            "/*\n\t日本語が\n\t続く\n*/",
            "/*\r\n\tx\r\n*/",
            "\t\t",
        ] {
            for rules in [CommentRules::Fenced, CommentRules::Indented] {
                let shape = CommentShape::parse(raw, rules);
                let _ = shape.source();
                let _ = GlossPlan::new(raw, rules).segments();
            }
        }
    }

    /// A fence still decides, and it decides first. Under these rules an
    /// indented fence line would otherwise open nothing, and the prose after it
    /// would be glossed inside what the writer marked as code.
    #[test]
    fn a_fence_outranks_indentation() {
        let shape = CommentShape::parse(
            "// Example:\n//\n//\t```\n// prose inside the fence\n//\t```",
            CommentRules::Indented,
        );

        assert_eq!(shape.units(), ["Example:"]);
        assert_eq!(
            shape.source(),
            "Example:\n\n\t```\nprose inside the fence\n\t```"
        );
    }

    /// The regression anchor: in a Rust comment indentation means nothing, and
    /// every one of these would lose its gloss if the rule fired outside the
    /// rules that ask for it. Measured, that is 235,382 blocks of Rust that do
    /// not move (`docs/model-runtime-notes.md` §16).
    #[test]
    fn indentation_in_a_rust_comment_is_not_an_example() {
        for (raw, units) in [
            (
                "///   Prose\n///     wrapped onto two lines.",
                vec!["Prose wrapped onto two lines."],
            ),
            (
                "/// Loads a user.\n///\n///     let user = find_user(id);",
                vec!["Loads a user.", "let user = find_user(id);"],
            ),
            (
                "/**\n *   Prose.\n *     More prose.\n */",
                vec!["Prose. More prose."],
            ),
            (
                "//\tTODO: indent means nothing here.",
                vec!["TODO: indent means nothing here."],
            ),
        ] {
            assert_eq!(
                CommentShape::parse(raw, CommentRules::Fenced).units(),
                units,
                "in {raw:?}"
            );
        }
    }
}
