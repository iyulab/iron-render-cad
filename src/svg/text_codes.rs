//! The control codes a drawing's text is written with, turned into the
//! characters they stand for.
//!
//! The model carries text as the file wrote it; this is where it is read.
//! Two families are handled, in one pass so that what one produces is never
//! read again as the other:
//!
//! - **Percent codes**, in TEXT, ATTRIB and MTEXT: `%%d` (degree), `%%p`
//!   (plus-minus), `%%c` (diameter), `%%%` (a percent sign), `%%nnn` (the
//!   character with that three-digit code); `%%u` / `%%o` switch underline /
//!   overline, which is not drawn, so they are dropped. Any other `%%x` is
//!   left as written.
//! - **MTEXT formatting codes**: `\P` (paragraph) and `\N` (column) break the
//!   line; `\~` is a non-breaking space; `\\`, `\{` and `\}` are the
//!   characters; `\U+XXXX` is the Unicode character; `\S…;` is stacked text,
//!   drawn inline as `top/bottom`; codes that take a value up to `;` (`\A`,
//!   `\C`, `\c`, `\F`, `\f`, `\H`, `\Q`, `\T`, `\W`, `\p`) and the one-letter
//!   switches (`\L`, `\l`, `\O`, `\o`, `\K`, `\k`) change only how text looks
//!   and are dropped, as are `{` / `}` grouping. `\M+nXXXX` (a character in
//!   an Asian code page) is left as written: this crate has no code page
//!   tables. Any other backslash is kept, with what follows it.
//!
//! What is drawn is plain text -- fonts, colors, heights and stacking are
//! not reproduced.

/// `text` with its percent codes read, and, for MTEXT, its formatting codes.
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
                .then(|| digits.parse::<u32>().ok().and_then(char::from_u32))
                .flatten()
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
        'P' | 'N' => out.push('\n'),
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
            out.extend(rest[1..end].iter().map(|&x| match x {
                '^' | '#' => '/',
                x => x,
            }));
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
    }
}
