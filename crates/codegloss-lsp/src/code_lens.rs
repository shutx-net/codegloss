//! Comment blocks rendered as code lenses.
//!
//! Zed draws a lens as a block placed *above* the line the lens points at
//! (`BlockPlacement::Above`), which is the one display mode that reproduces the
//! mock-up in Issue #1: the Japanese on its own line, the English comment
//! directly underneath it. Two properties of that renderer decide the shape of
//! everything here:
//!
//! - The only text Zed takes from a lens is `command.title`. A lens without a
//!   command, or with an empty title, is not drawn at all - hence
//!   [`NOOP_COMMAND`], a command that exists purely so that a lens has one.
//! - The block is one line high, and lenses that land on the same line are
//!   joined with `" | "`. A title is therefore always a single line, and one
//!   comment block produces exactly one lens.
//!
//! # Why a lens says "translating" while hover does not
//!
//! Hover falls back to the English source while a gloss is still being
//! produced; a lens shows [`PENDING_TITLE`] instead. The inconsistency is
//! deliberate, and it follows from where the two are drawn:
//!
//! - A lens sits directly above the comment it glosses. Echoing the English
//!   there would stack the same sentence twice on adjacent lines - noise, not
//!   information. A hover popup covers the code, so the English in it is the
//!   only copy the reader sees.
//! - A lens can be replaced after the fact: `workspace/codeLens/refresh` makes
//!   the editor refetch, so the placeholder lives for as long as one batch
//!   takes. The protocol has no `workspace/hover/refresh`, so a placeholder in
//!   a popup would stay wrong until the user hovered again.
//! - Emitting no lens at all until the gloss arrives would insert a line into
//!   the buffer the moment it does, and the code under the cursor would jump.
//!   A placeholder reserves the line up front.

use codegloss_core::CommentBlock;
use tower_lsp_server::ls_types::{CodeLens, Command, Position, Range};

/// Identifier of the command every lens carries.
///
/// It does nothing on purpose. Zed refuses to draw a lens whose `command` is
/// absent, and a drawn lens is clickable, so the server has to accept
/// `workspace/executeCommand` for this name and answer with nothing.
pub const NOOP_COMMAND: &str = "codegloss.noop";

/// Shown on a lens whose gloss has not been produced yet.
///
/// See the module docs for why this is a placeholder here and the English
/// source in a hover.
pub const PENDING_TITLE: &str = "⟳ 翻訳中…";

/// Longest title emitted, counted in characters and including [`ELLIPSIS`].
///
/// The block a lens is drawn in is one line high, so a long gloss cannot wrap:
/// it would run off the edge of the editor. Cutting it keeps the line readable
/// and the full text stays one hover away.
const MAX_TITLE_CHARS: usize = 120;

/// Marks a title cut at [`MAX_TITLE_CHARS`].
const ELLIPSIS: char = '…';

/// Stands in for `|` inside a title.
///
/// `" | "` is what Zed puts between two lenses on one line, so a bar in a gloss
/// reads as a break between two of them. The fullwidth form looks the same and
/// cannot be confused with the separator.
const BAR_REPLACEMENT: char = '｜';

/// A lens for a block that has been glossed.
pub fn glossed(block: &CommentBlock, gloss: &str) -> CodeLens {
    let mut title = single_line(gloss);
    if title.is_empty() {
        // An engine that returned nothing would leave the line blank, and a
        // blank title is a lens Zed does not draw. Falling back to the source
        // is what hover does with a missing gloss, and it keeps the line.
        title = single_line(&block.text);
    }
    lens(block, title)
}

/// A lens for a block whose gloss is still being produced.
pub fn pending(block: &CommentBlock) -> CodeLens {
    lens(block, PENDING_TITLE.to_owned())
}

fn lens(block: &CommentBlock, title: String) -> CodeLens {
    // IMPORTANT: the line has to be the comment's own first line. The block is
    // placed above it, so pointing one line higher would put the gloss above
    // the code that precedes the comment instead of above the comment.
    //
    // Two comment blocks that begin on the same line - `/* a */ /* b */` - get
    // a lens each, and Zed joins them into one line with `" | "`. That is the
    // reason a title never contains a bar of its own.
    let position = Position {
        line: block.start_line,
        character: 0,
    };

    CodeLens {
        range: Range {
            start: position,
            end: position,
        },
        command: Some(Command {
            title,
            command: NOOP_COMMAND.to_owned(),
            arguments: None,
        }),
        // `resolve_provider` is advertised as false: a lens arrives complete
        // and `codeLens/resolve` is never sent, so there is nothing to carry.
        data: None,
    }
}

/// Flattens a gloss into something that can be drawn on a single line.
///
/// Every run of whitespace - line breaks included - collapses to one space, and
/// the result is cut to [`MAX_TITLE_CHARS`]. A newline left in place does not
/// grow the block to two lines; it is simply drawn as whatever the renderer
/// makes of a control character.
fn single_line(text: &str) -> String {
    let mut title = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !title.is_empty() {
            title.push(' ');
        }
        for character in word.chars() {
            title.push(if character == '|' {
                BAR_REPLACEMENT
            } else {
                character
            });
        }
    }

    if title.chars().count() <= MAX_TITLE_CHARS {
        return title;
    }
    // Cut on characters, never on bytes: the glosses this cuts are Japanese,
    // and a byte offset lands in the middle of one.
    title
        .chars()
        .take(MAX_TITLE_CHARS - 1)
        .chain(std::iter::once(ELLIPSIS))
        .collect()
}

#[cfg(test)]
mod tests {
    use codegloss_core::CommentStyle;

    use super::*;

    fn block(start_line: u32, text: &str) -> CommentBlock {
        CommentBlock {
            style: CommentStyle::Line,
            text: text.to_owned(),
            raw: format!("// {text}"),
            start_line,
            end_line: start_line,
            start_byte: 0,
            end_byte: text.len() + 3,
        }
    }

    fn title_of(lens: &CodeLens) -> &str {
        &lens
            .command
            .as_ref()
            .expect("every lens has a command")
            .title
    }

    /// The lens points at the comment's own line, and at column zero: Zed keys
    /// the block off the line, and the character is what its indentation is
    /// measured from.
    #[test]
    fn a_lens_points_at_the_first_line_of_its_block() {
        let lens = glossed(&block(7, "Return the cached user."), "キャッシュを返す。");

        assert_eq!(lens.range.start, Position::new(7, 0));
        assert_eq!(lens.range.end, lens.range.start);
        assert_eq!(title_of(&lens), "キャッシュを返す。");
    }

    /// No command means no lens on screen, so both kinds carry one.
    #[test]
    fn every_lens_carries_the_noop_command() {
        for lens in [
            glossed(&block(0, "Note."), "注記。"),
            pending(&block(0, "Note.")),
        ] {
            let command = lens.command.expect("every lens has a command");
            assert_eq!(command.command, NOOP_COMMAND);
            assert_eq!(command.arguments, None);
            assert!(!command.title.is_empty(), "an empty title is never drawn");
        }
    }

    #[test]
    fn a_block_without_a_gloss_shows_the_placeholder() {
        assert_eq!(title_of(&pending(&block(3, "Note."))), PENDING_TITLE);
    }

    /// The lens block is one line high, so a multi-line gloss has to be folded
    /// into one line rather than left to the renderer.
    #[test]
    fn line_breaks_and_runs_of_spaces_collapse_to_single_spaces() {
        assert_eq!(
            single_line("first line\nsecond   line\r\n\tthird"),
            "first line second line third"
        );
        assert_eq!(single_line("  padded  "), "padded");
        assert_eq!(single_line("   "), "");
    }

    /// `" | "` is Zed's separator between two lenses on one line.
    #[test]
    fn a_vertical_bar_is_replaced_so_it_cannot_read_as_a_separator() {
        assert_eq!(single_line("a | b"), "a ｜ b");
    }

    #[test]
    fn a_long_gloss_is_cut_with_an_ellipsis() {
        let title = single_line(&"あ".repeat(MAX_TITLE_CHARS * 2));

        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.ends_with(ELLIPSIS));
        // Counting characters rather than bytes is the point: cutting this at a
        // byte offset would split a character in half.
        assert!(title.starts_with("あああ"));
    }

    #[test]
    fn a_gloss_that_fits_is_left_alone() {
        let text = "x".repeat(MAX_TITLE_CHARS);
        assert_eq!(single_line(&text), text);
    }

    /// An engine that returns an empty string must not produce an invisible
    /// lens; the source keeps the line occupied.
    #[test]
    fn an_empty_gloss_falls_back_to_the_source_text() {
        assert_eq!(title_of(&glossed(&block(0, "Note."), "   ")), "Note.");
    }
}
