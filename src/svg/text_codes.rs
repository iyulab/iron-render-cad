//! The control codes a drawing's text is written with, turned into the
//! characters they stand for.
//!
//! The model carries text with its codes in it, and splits it into them
//! ([`uncad_model::text::tokens`], one pass, so what one code produces is
//! never read again as another); this is what the renderer draws of each.
//! How a file *stored* a string (`\U+XXXX`, `\M+nXXXX`) is not here: the
//! reader undid it before the text reached the model. Two families are
//! drawn:
//!
//! - **Percent codes**, in TEXT, ATTRIB and MTEXT: `%%d` (degree), `%%p`
//!   (plus-minus), `%%c` (diameter), `%%%` (a percent sign), `%%nnn` (the
//!   character with that three-digit code); `%%u` / `%%o` switch underline /
//!   overline, which is not drawn, so they are dropped. Codes 127, 128 and
//!   129 are where the standard shape fonts keep the degree, plus-minus and
//!   diameter signs (dimension text writes `90%%127` for 90 degrees), so
//!   they are read as those signs; a code naming any other control
//!   character, which drawn text cannot carry, is left as written, as is
//!   any other `%%x`. Outside MTEXT a backslash starts no code.
//! - **MTEXT formatting codes**: `\P` (paragraph), `\N` (column) and `\X` (a
//!   dimension's text, split above and below its line) break the line; `\~`
//!   is a non-breaking space; `\\`, `\{` and `\}` are the characters; `\S…;`
//!   is stacked text, drawn inline as `top/bottom` -- one side alone when the
//!   other is empty (a superscript or a subscript), and set off by a space
//!   after a digit, so that `3\S1#2;` reads `3 1/2` and not thirty-one
//!   halves; codes that take a value up to `;` (`\A`, `\C`, `\c`, `\F`,
//!   `\f`, `\H`, `\Q`, `\T`, `\W`, `\p`) and the one-letter switches (`\L`,
//!   `\l`, `\O`, `\o`, `\K`, `\k`) change only how text looks and are
//!   dropped, as are `{` / `}` grouping. Any other backslash is kept, with
//!   what follows it.
//!
//! What is drawn is plain text -- fonts, colors, heights and stacking are
//! not reproduced.

/// `text` as the plain text it draws: its percent codes and, for MTEXT,
/// its formatting codes read (see the module documentation).
pub(super) fn decode(text: &str, mtext: bool) -> String {
    let kind = if mtext {
        TextKind::MText
    } else {
        TextKind::Line
    };
    let mut out = String::with_capacity(text.len());
    for token in tokens(text, kind) {
        match token {
            Token::Char(c) => out.push(c),
            Token::Special(Special::Degree) => out.push('\u{b0}'),
            Token::Special(Special::PlusMinus) => out.push('\u{b1}'),
            Token::Special(Special::Diameter) => out.push('\u{2300}'),
            Token::CharCode(code) => match shape_font_char(u32::from(code)) {
                Some(c) => out.push(c),
                None => out.push_str(&format!("%%{code:03}")),
            },
            Token::Break(_) => out.push('\n'),
            Token::NonBreakingSpace => out.push(' '),
            Token::Stack { top, bottom, .. } => push_stack(top, bottom, &mut out),
            Token::StackUnsplit(body) => push_stack(body, "", &mut out),
            Token::Unknown(raw) => out.push_str(raw),
            // How the text looks, which is not drawn.
            Token::Toggle(_) | Token::Property { .. } | Token::GroupStart | Token::GroupEnd => {}
        }
    }
    out
}

/// The character a `%%nnn` code draws: the standard shape fonts' degree,
/// plus-minus and diameter signs at 127, 128 and 129, otherwise the
/// character with that code -- unless it is a control character, which
/// has no place in drawn text.
fn shape_font_char(code: u32) -> Option<char> {
    match code {
        127 => Some('\u{b0}'),
        128 => Some('\u{b1}'),
        129 => Some('\u{2300}'),
        _ => char::from_u32(code).filter(|c| !c.is_control()),
    }
}

/// Writes a stack inline as `top/bottom`. A stack with one side empty -- a
/// superscript `2^`, a subscript `^2` -- is the other side alone, not a
/// fraction with a dangling bar; and a stack that follows a digit is set off
/// from it by a space, since `3` and `1/2` written together read as `31/2`.
fn push_stack(top: &str, bottom: &str, out: &mut String) {
    let blank = |side: &str| side.chars().all(char::is_whitespace);
    let shown = match (blank(top), blank(bottom)) {
        (true, true) => return,
        (true, false) => bottom.to_string(),
        (false, true) => top.to_string(),
        (false, false) => format!("{top}/{bottom}"),
    };
    if out.ends_with(|c: char| c.is_ascii_digit()) {
        out.push(' ');
    }
    out.push_str(&shown);
}

use uncad_model::text::{tokens, Special, TextKind, Token};

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn percent_codes_are_the_characters_they_stand_for_in_any_text() {
        assert_eq!(
            decode("%%c32 %%d %%p0.1 100%%%", false),
            "\u{2300}32 \u{b0} \u{b1}0.1 100%"
        );
        assert_eq!(decode("%%C32", true), "\u{2300}32");
        assert_eq!(decode("%%065%%066", false), "AB");
        assert_eq!(decode("%%uUNDER%%u", false), "UNDER");
        // Not a code: kept as written.
        assert_eq!(decode("50%% off %%x %%12", false), "50%% off %%x %%12");
    }

    #[test]
    fn a_storage_escape_is_not_read_here() {
        // How a file stored a character (`\U+XXXX`) is undone by the reader
        // before the text reaches the model; what is left is an escape of an
        // ASCII character, which the reader keeps as written -- and so does
        // the renderer, in either kind of text.
        assert_eq!(decode(r"\U+0041 20", false), r"\U+0041 20");
        assert_eq!(decode(r"\A1;\U+005C", true), r"\U+005C");
        // The character the reader produced is drawn as itself.
        assert_eq!(decode("\\A1;\u{2205}2.3794", true), "\u{2205}2.3794");
    }

    #[test]
    fn an_escaped_backslash_is_a_backslash_and_starts_no_code() {
        assert_eq!(decode(r"C:\\PATH\\U+0041", true), r"C:\PATH\U+0041");
        assert_eq!(decode(r"a\\b", true), r"a\b");
        assert_eq!(decode(r"\{braces\}", true), "{braces}");
    }

    #[test]
    fn formatting_codes_are_dropped_and_structure_kept() {
        assert_eq!(
            decode(r"\A1;Line1\PLine2 {\C1;colored} \S1#8;", true),
            "Line1\nLine2 colored 1/8"
        );
        assert_eq!(decode(r"\fArial|b1|i0;bold\Lu\l", true), "boldu");
        assert_eq!(decode(r"a\~b", true), "a b");
    }

    #[test]
    fn an_unclosed_or_unknown_code_keeps_its_text() {
        // Without a `;` the height code does not end: nothing is swallowed.
        assert_eq!(decode(r"\H2.5 tall", true), r"\H2.5 tall");
        assert_eq!(decode(r"\Z?", true), r"\Z?");
        assert_eq!(decode(r"\M+18140", true), r"\M+18140");
    }

    #[test]
    fn plain_text_is_not_read_as_mtext() {
        assert_eq!(decode(r"\P{x}", false), r"\P{x}");
        assert_eq!(decode(r"C:\\PATH", false), r"C:\\PATH");
    }

    #[test]
    fn the_shape_font_codes_are_degree_plus_minus_and_diameter() {
        // As dimension text writes them.
        assert_eq!(decode("90%%127", false), "90\u{b0}");
        assert_eq!(decode("%%1292.0000", false), "\u{2300}2.0000");
        assert_eq!(decode("%%128", false), "\u{b1}");
        // A control character cannot be drawn: the code stays as written.
        assert_eq!(decode("a%%001b", false), "a%%001b");
    }

    #[test]
    fn outside_mtext_a_backslash_starts_nothing() {
        assert_eq!(decode(r"C:\Temp\{x}", false), r"C:\Temp\{x}");
    }

    #[test]
    fn a_dimensions_text_below_its_line_is_a_line_of_its_own() {
        assert_eq!(decode(r"A\PB\XC\ND", true), "A\nB\nC\nD");
    }

    #[test]
    fn a_fraction_after_a_number_keeps_the_number_readable() {
        // Three and a half inches, not thirty-one halves.
        assert_eq!(decode(r#"3{\H0.7x;\S1#2;}""#, true), r#"3 1/2""#);
        assert_eq!(
            decode(r#"\A1;1'-1{\H0.750000x;\S1#8;}""#, true),
            r#"1'-1 1/8""#
        );
        assert_eq!(decode(r#"\A1;{\H0.750000x;\S1#2;}""#, true), r#"1/2""#);
        assert_eq!(decode(r#"\A1;7'-4""#, true), r#"7'-4""#);
    }

    #[test]
    fn every_stack_separator_is_a_bar_and_a_one_sided_stack_is_its_side() {
        assert_eq!(decode(r"\S1/2;", true), "1/2");
        assert_eq!(decode(r"\S1#2;", true), "1/2");
        assert_eq!(decode(r"\S+0.1^-0.2;", true), "+0.1/-0.2");
        // A superscript and a subscript are stacks with one side empty.
        assert_eq!(decode(r"m\S2^;", true), "m2");
        assert_eq!(decode(r"H\S^2;", true), "H2");
        assert_eq!(decode(r"\Sabc;", true), "abc");
        assert_eq!(decode(r"x\S^;y", true), "xy");
    }

    #[test]
    fn paragraph_and_character_codes_leave_only_the_text() {
        assert_eq!(decode(r"ALPHA\P\PBRAVO", true), "ALPHA\n\nBRAVO");
        assert_eq!(
            decode(
                r"\pi102.25;{\f@Arial Unicode MS|b1|i0|c0|p34;A T M O S}",
                true
            ),
            "A T M O S"
        );
        assert_eq!(decode(r"\C1;red\C256;bylayer", true), "redbylayer");
        assert_eq!(decode(r"\fArial|b0|i0;\W0.8;\Q10;\T1.2;\H2.5;x", true), "x");
        assert_eq!(
            decode(r"\Lunder\l \Oover\o \Kstrike\k", true),
            "under over strike"
        );
        assert_eq!(decode("trailing\\", true), "trailing\\");
        assert_eq!(
            decode("\u{BC29} 101\\P\u{BA74}\u{C801} 32.5\u{33A1}", true),
            "\u{BC29} 101\n\u{BA74}\u{C801} 32.5\u{33A1}"
        );
    }

    #[test]
    fn percent_codes_read_the_same_in_a_text() {
        assert_eq!(decode("108%%d", false), "108\u{b0}");
        assert_eq!(decode("%%P0.5", false), "\u{b1}0.5");
        assert_eq!(decode("%%UBOOK RETURN%%U", false), "BOOK RETURN");
        assert_eq!(decode("%%176", false), "\u{b0}");
        // A code naming a control character is left as written.
        assert_eq!(decode("a%%009b", false), "a%%009b");
        assert_eq!(decode(r#"\A1;2"%%C"#, true), "2\"\u{2300}");
        // Not codes.
        assert_eq!(decode("50%", false), "50%");
        assert_eq!(decode("a%%zb", false), "a%%zb");
    }
}
