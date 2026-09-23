//! The control codes a drawing's text is written with, turned into the
//! characters they stand for.
//!
//! The model carries text as the file wrote it; this is where it is read.
//! Three families are handled, in one pass so that what one produces is
//! never read again as another:
//!
//! - **Percent codes**, in TEXT, ATTRIB and MTEXT: `%%d` (degree), `%%p`
//!   (plus-minus), `%%c` (diameter), `%%%` (a percent sign), `%%nnn` (the
//!   character with that three-digit code); `%%u` / `%%o` switch underline /
//!   overline, which is not drawn, so they are dropped. Codes 127, 128 and
//!   129 are where the standard shape fonts keep the degree, plus-minus and
//!   diameter signs (dimension text writes `90%%127` for 90 degrees), so
//!   they are read as those signs; a code naming any other control
//!   character, which drawn text cannot carry, is left as written, as is
//!   any other `%%x`.
//! - **Unicode escapes**, in any text: `\U+XXXX` is the Unicode character --
//!   the way a DXF file writes a character its code page cannot hold, in
//!   every string and not only in MTEXT, so a drawing saved both ways reads
//!   the same. Outside MTEXT no other backslash starts a code.
//! - **MTEXT formatting codes**: `\P` (paragraph), `\N` (column) and `\X` (a
//!   dimension's text, split above and below its line) break the line; `\~`
//!   is a non-breaking space; `\\`, `\{` and `\}` are the characters; `\S…;`
//!   is stacked text, drawn inline as `top/bottom` -- one side alone when the
//!   other is empty (a superscript or a subscript), and set off by a space
//!   after a digit, so that `3\S1#2;` reads `3 1/2` and not thirty-one
//!   halves; codes that take a value up to `;` (`\A`, `\C`, `\c`, `\F`,
//!   `\f`, `\H`, `\Q`, `\T`, `\W`, `\p`) and the one-letter switches (`\L`,
//!   `\l`, `\O`, `\o`, `\K`, `\k`) change only how text looks and are
//!   dropped, as are `{` / `}` grouping. `\M+nXXXX` (a character in an Asian
//!   code page) is left as written: this crate has no code page tables. Any
//!   other backslash is kept, with what follows it.
//!
//! What is drawn is plain text -- fonts, colors, heights and stacking are
//! not reproduced.

/// `text` with its percent codes and Unicode escapes read, and, for MTEXT,
/// its formatting codes.
pub(super) fn decode(text: &str, mtext: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '%' && chars.get(i + 1) == Some(&'%') {
            i += 2 + percent_code(&chars[i + 2..], &mut out);
            continue;
        }
        if c == '\\' && !mtext && chars.get(i + 1) == Some(&'U') {
            if let Some(ch) = unicode_escape(&chars[i + 2..]) {
                out.push(ch);
                i += 7;
                continue;
            }
        }
        if mtext {
            match c {
                '\\' => {
                    i += 1 + mtext_code(&chars[i + 1..], &mut out);
                    continue;
                }
                '{' | '}' => {
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Reads the percent code after a `%%`, pushing what it stands for, and
/// returns how many characters it took.
fn percent_code(rest: &[char], out: &mut String) -> usize {
    match rest.first().map(char::to_ascii_lowercase) {
        Some('d') => out.push('\u{b0}'),
        Some('p') => out.push('\u{b1}'),
        Some('c') => out.push('\u{2300}'),
        Some('%') => out.push('%'),
        Some('u' | 'o') => {}
        Some(d) if d.is_ascii_digit() => {
            let digits: String = rest.iter().take(3).collect();
            match (digits.len() == 3 && digits.chars().all(|d| d.is_ascii_digit()))
                .then(|| digits.parse::<u32>().ok())
                .flatten()
                .and_then(shape_font_char)
            {
                Some(ch) => {
                    out.push(ch);
                    return 3;
                }
                None => {
                    out.push_str("%%");
                    return 0;
                }
            }
        }
        _ => {
            out.push_str("%%");
            return 0;
        }
    }
    1
}

/// Reads the MTEXT code after a `\`, pushing what it stands for, and
/// returns how many characters it took.
fn mtext_code(rest: &[char], out: &mut String) -> usize {
    let Some(&c) = rest.first() else {
        out.push('\\');
        return 0;
    };
    match c {
        'P' | 'N' | 'X' => out.push('\n'),
        '~' => out.push(' '),
        '\\' | '{' | '}' => out.push(c),
        'L' | 'l' | 'O' | 'o' | 'K' | 'k' => {}
        'U' => {
            if let Some(ch) = unicode_escape(&rest[1..]) {
                out.push(ch);
                return 6;
            }
            out.push_str("\\U");
        }
        'S' => {
            let Some(end) = rest.iter().position(|&x| x == ';') else {
                out.push_str("\\S");
                return 1;
            };
            push_stack(&rest[1..end], out);
            return end + 1;
        }
        'A' | 'C' | 'c' | 'F' | 'f' | 'H' | 'Q' | 'T' | 'W' | 'p' => {
            // The value runs to the next `;`; without one the code is not
            // closed, and the text is kept rather than swallowed.
            match rest.iter().position(|&x| x == ';') {
                Some(end) => return end + 1,
                None => {
                    out.push('\\');
                    out.push(c);
                }
            }
        }
        _ => {
            out.push('\\');
            out.push(c);
        }
    }
    1
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

/// Writes a stack's body inline as `top/bottom`, whichever of `^`, `#` and
/// `/` separates the two. A stack with one side empty -- a superscript
/// `2^`, a subscript `^2` -- is the other side alone, not a fraction with a
/// dangling bar; and a stack that follows a digit is set off from it by a
/// space, since `3` and `1/2` written together read as `31/2`.
fn push_stack(body: &[char], out: &mut String) {
    let blank = |side: &[char]| side.iter().all(|x| x.is_whitespace());
    let shown: Vec<char> = match body.iter().position(|&x| matches!(x, '^' | '#' | '/')) {
        Some(at) if blank(&body[..at]) => body[at + 1..].to_vec(),
        Some(at) if blank(&body[at + 1..]) => body[..at].to_vec(),
        _ => body
            .iter()
            .map(|&x| match x {
                '^' | '#' => '/',
                x => x,
            })
            .collect(),
    };
    if blank(&shown) {
        return;
    }
    if out.ends_with(|c: char| c.is_ascii_digit()) {
        out.push(' ');
    }
    out.extend(shown);
}

/// `+XXXX` (four hex digits) as the character it names.
fn unicode_escape(rest: &[char]) -> Option<char> {
    if rest.first() != Some(&'+') || rest.len() < 5 {
        return None;
    }
    let hex: String = rest[1..5].iter().collect();
    if !hex.chars().all(|h| h.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
}

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
    fn a_unicode_escape_is_its_character_whether_or_not_a_semicolon_follows() {
        // Measured in a drawing: a diameter symbol written as an escape right
        // after an alignment code, with the number running on.
        assert_eq!(decode(r"\A1;\U+22052.3794", true), "\u{2205}2.3794");
        assert_eq!(decode(r"\U+00B0C; next", true), "\u{b0}C; next");
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
    fn a_unicode_escape_is_its_character_in_plain_text_too() {
        // An older DXF file writes a character its code page cannot hold
        // this way in every string, TEXT and ATTRIB included.
        assert_eq!(decode(r"\U+2205 20", false), "\u{2205} 20");
        assert_eq!(decode(r"\U+04100", false), "\u{410}0");
        assert_eq!(decode(r"\U+12", false), r"\U+12");
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
    fn a_unicode_escape_is_its_character_in_a_text_too() {
        assert_eq!(decode(r"\U+00B1 3", false), "\u{b1} 3");
        assert_eq!(decode(r"\U+2205 50", true), "\u{2205} 50");
        // Not an escape: kept, backslash and all.
        assert_eq!(decode(r"\U+ZZZZ", false), r"\U+ZZZZ");
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
            decode("방 101\\P면적 32.5\u{33A1}", true),
            "방 101\n면적 32.5\u{33A1}"
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
