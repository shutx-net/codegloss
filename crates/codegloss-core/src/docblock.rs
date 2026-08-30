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
//! Consecutive prose lines are still merged into one unit, as P3 decided: a
//! sentence spread over three `//` lines has to reach the engine whole.
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

/// Openers of a block comment, longest first so that `/**` is not read as `/*`.
const BLOCK_OPENERS: [&str; 3] = ["/**", "/*!", "/*"];
/// Markers of a line comment, longest first.
const LINE_MARKERS: [&str; 3] = ["///", "//!", "//"];
/// Closer of a block comment.
const BLOCK_CLOSER: &str = "*/";
/// Markdown code fences. Everything between two of them is copied through.
const FENCES: [&str; 2] = ["```", "~~~"];
/// Tags whose first argument is an identifier by definition, never prose.
const TAGS_WITH_A_NAME: [&str; 4] = ["@param", "@throws", "@exception", "@arg"];

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

            if fenced || FENCES.iter().any(|fence| content.starts_with(fence)) {
                if FENCES.iter().any(|fence| content.starts_with(fence)) {
                    fenced = !fenced;
                }
                flush(&mut paragraph, &mut pieces);
                pieces.push(Piece::Verbatim(content.to_owned()));
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
    units: Vec<Masked>,
}

impl GlossPlan {
    /// Prepares the comment written as `raw`.
    pub fn new(raw: &str) -> Self {
        let shape = CommentShape::parse(raw);
        let units = shape.units().into_iter().map(mask).collect();
        Self { shape, units }
    }

    /// What the engine is asked to translate: one masked segment per unit.
    pub fn segments(&self) -> Vec<Segment> {
        self.units
            .iter()
            .map(|unit| Segment::new(unit.masked()))
            .collect()
    }

    /// Whether there is anything to translate at all.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// The English prose in the block's own structure.
    pub fn source(&self) -> String {
        self.shape.source()
    }

    /// Puts the translations back: placeholders first, then the structure.
    ///
    /// `translations` has one entry per [`Self::segments`], in the same order.
    /// A batch of the wrong length is a broken engine rather than a bad
    /// translation, so the block falls back to its English wholesale; a single
    /// unit that lost a placeholder falls back on its own, in
    /// [`Masked::unmask`].
    pub fn restore(&self, translations: &[String]) -> String {
        if translations.len() != self.units.len() {
            return self.shape.source();
        }

        let restored: Vec<String> = self
            .units
            .iter()
            .zip(translations)
            .map(|(unit, translated)| unit.unmask(translated))
            .collect();
        self.shape.rebuild(&restored)
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
fn strip_markers(line: &str, block: bool, first: bool) -> &str {
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

    content.trim()
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

    /// P3 merges a run of `//` lines into one sentence, and that decision holds
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

    /// The property the whole phase exists for: with an engine that returns its
    /// input, a block comes back exactly as its prose went in.
    #[test]
    fn a_passthrough_translation_restores_the_source_exactly() {
        for raw in [
            JAVADOC,
            "// Return the cached user.",
            "/// Returns `UserDetails` when authentication succeeds.",
            "/// See https://example.com/docs for the protocol.",
            "// TODO: replace find_user with UserRepository::load().",
            "/// Loads a user.\n///\n/// ```\n/// let user = find_user(id);\n/// ```",
        ] {
            let plan = GlossPlan::new(raw);
            let translations: Vec<String> = plan
                .segments()
                .iter()
                .map(|segment| segment.text().to_owned())
                .collect();

            assert_eq!(plan.restore(&translations), plan.source(), "in {raw:?}");
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
