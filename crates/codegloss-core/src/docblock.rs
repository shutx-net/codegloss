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
//! [`CommentBlock::text`]: crate::CommentBlock::text
//! [`CommentBlock::raw`]: crate::CommentBlock::raw

use crate::Segment;
use crate::preserve::{Masked, mask};
use crate::sentence::{engine_form, join_sentences, split_sentences};

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
    /// one, or a tag with no prose after it.
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
    pub fn parse(raw: &str) -> Self {
        let block = raw.trim_start().starts_with("/*");
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
    pub fn new(raw: &str) -> Self {
        let shape = CommentShape::parse(raw);
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
            CommentShape::parse(JAVADOC).units(),
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
            CommentShape::parse(JAVADOC).source(),
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
        let shape = CommentShape::parse(JAVADOC);
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
        let plan =
            GlossPlan::new("/// Returns the user. Fails when `id` is unknown. Nothing is cached.");
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
        let plan = GlossPlan::new("/// Returns the user. Fails when `id` is unknown.");
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
        let plan = GlossPlan::new("/// Returns the user. Nothing is cached.");
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
        let shape = CommentShape::parse("// Return the cached user\n// if there is one.");
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
            assert_eq!(CommentShape::parse(raw).units(), ["Note."], "in {raw:?}");
        }
    }

    #[test]
    fn a_blank_line_separates_two_paragraphs() {
        let shape = CommentShape::parse("/// One.\n///\n/// Two.");
        assert_eq!(shape.units(), ["One.", "Two."]);
        assert_eq!(shape.source(), "One.\n\nTwo.");
    }

    #[test]
    fn a_named_tag_keeps_its_argument_out_of_the_translation() {
        let shape = CommentShape::parse("/// @param id the user to load");
        assert_eq!(shape.units(), ["the user to load"]);
        assert_eq!(shape.source(), "@param id the user to load");
    }

    #[test]
    fn a_tag_without_prose_is_copied_through() {
        let shape = CommentShape::parse("/// @deprecated");
        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "@deprecated");
    }

    #[test]
    fn a_list_item_and_a_heading_keep_their_markers() {
        let shape =
            CommentShape::parse("/// # Panics\n///\n/// - Fails on a miss.\n/// 1. Then this.");
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
        let shape = CommentShape::parse(concat!(
            "/// Loads a user.\n",
            "///\n",
            "/// ```\n",
            "/// let user = find_user(id);\n",
            "/// ```",
        ));

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
        let shape = CommentShape::parse(concat!(
            "/// ```\n",
            "/// a = 1;\n",
            "///\n",
            "/// b = 2;\n",
            "/// ```",
        ));

        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "```\na = 1;\n\nb = 2;\n```");
    }

    /// Issue #55: the nesting of an example is the example. A doctest whose
    /// body came back flush against the left margin is the code the reader is
    /// looking at, rewritten.
    #[test]
    fn the_indentation_inside_a_fence_is_kept() {
        let shape = CommentShape::parse(concat!(
            "/// ```\n",
            "/// if let Some(user) = find_user(id) {\n",
            "///     println!(\"{user}\");\n",
            "///     log(user);\n",
            "/// }\n",
            "/// ```",
        ));

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
        let shape = CommentShape::parse(concat!(
            "/// ```\n",
            "///     four();\n",
            "/// one();\n",
            "///none();\n",
            "/// ```",
        ));

        assert_eq!(shape.source(), "```\n    four();\none();\nnone();\n```");
    }

    /// The same rule under a Javadoc star. Neither corpus behind
    /// `docs/model-runtime-notes.md` §14 holds a starred block comment with a
    /// fence in it, so this is the only place the shape is pinned at all.
    #[test]
    fn a_starred_block_comment_keeps_the_indentation_inside_its_fence() {
        let spaced = CommentShape::parse("/**\n * ```\n *     y();\n * ```\n */");
        assert_eq!(spaced.source(), "```\n    y();\n```");

        // The space after the star is optional, and its absence is not
        // indentation either: what the star sheds is the star.
        let tight = CommentShape::parse("/**\n *```\n *     y();\n *```\n */");
        assert_eq!(tight.source(), "```\n    y();\n```");
    }

    /// A tab is one character of indentation and cannot be counted as spaces.
    /// `regex-syntax` writes its `hir` doctests this way.
    #[test]
    fn a_tab_inside_a_fence_is_content() {
        let shape = CommentShape::parse(concat!(
            "/// ```\n",
            "///\tif x {\n",
            "///\t\ty();\n",
            "/// }\n",
            "/// ```",
        ));

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
            CommentShape::parse("/// ```\r\n///     let x = 1;\r\n/// ```").source(),
            "```\n    let x = 1;\n```"
        );
        assert_eq!(
            CommentShape::parse("/// ```\n///     日本語\n/// ```").source(),
            "```\n    日本語\n```"
        );
        assert_eq!(
            CommentShape::parse("/// ```\n///\u{3000}全角\n/// ```").source(),
            "```\n\u{3000}全角\n```"
        );
    }

    /// The other side of the rule: outside a fence the indentation is not
    /// content, and a paragraph is glossed as one line however its
    /// continuation lines were laid out.
    #[test]
    fn prose_lines_are_still_trimmed() {
        let shape = CommentShape::parse("///   Prose   \n///     wrapped onto two lines.");
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
        let shape = CommentShape::parse("/*!\n```\nif x {\n    y();\n}\n```\n*/");
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
            assert_eq!(CommentShape::parse(raw).units(), [unit], "in {raw:?}");
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
        let shape = CommentShape::parse(concat!(
            "/// ```text\n",
            "/// #[derive(Copy, Clone)]\n",
            "///   ~~~~~~Path\n",
            "///   ^^^^^^^^^^^^^^^^^^^Meta::List\n",
            "/// ```",
        ));

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
        let shape = CommentShape::parse("/* Loads the user\n   from the cache. */");
        assert_eq!(shape.units(), ["Loads the user from the cache."]);
    }

    #[test]
    fn a_comment_with_no_prose_has_no_units() {
        let shape = CommentShape::parse("//");
        assert!(shape.units().is_empty());
        assert_eq!(shape.source(), "");
    }

    #[test]
    fn a_plan_hands_the_engine_masked_segments() {
        let plan = GlossPlan::new("/// Returns `UserDetails` when authentication succeeds.");
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
            let plan = GlossPlan::new(raw);
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
            let plan = GlossPlan::new(raw);
            assert_eq!(plan.restore(&plan.sources()), plan.source(), "in {raw:?}");
        }
    }

    /// An engine that answers with the wrong number of translations is a broken
    /// engine; pairing them up anyway would gloss each line with its neighbour.
    #[test]
    fn a_batch_of_the_wrong_length_falls_back_to_the_source() {
        let plan = GlossPlan::new(JAVADOC);
        assert_eq!(plan.restore(&["訳".to_owned()]), plan.source());
    }

    #[test]
    fn a_unit_that_lost_a_placeholder_falls_back_on_its_own() {
        let plan = GlossPlan::new("/// Returns `UserDetails`.\n///\n/// @return the user");
        let gloss = plan.restore(&["返します。".to_owned(), "ユーザー".to_owned()]);

        // The first unit lost `X0Q` and keeps its English; the second is fine.
        assert_eq!(gloss, "Returns `UserDetails`.\n\n@return ユーザー");
    }
}
