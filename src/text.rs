//! Decoding of the inline codes AutoCAD stores in text into what a reader
//! sees: MTEXT's `\P` paragraph breaks, `\S` stacked fractions, `{...}`
//! groups and `\A`/`\H`/`\f`-style format codes, and the `%%c` / `%%d` /
//! `%%p` symbol codes and `%%u`/`%%o` toggles TEXT and ATTRIB use.
//!
//! The model keeps the string as the file wrote it; turning it into the
//! characters to draw is this renderer's job. Getting it right matters for
//! numbers: `3{\H0.7x;\S1#2;}"` is three and a half inches, and stripping the
//! codes the way a regular expression can read it as `31/2"`.
//!
//! Not attempted: any visual formatting (fonts, colours, heights, stacking
//! drawn as a stack), `\M+nXXXX` multibyte escapes, and paragraph properties
//! beyond dropping them.

/// The characters an MTEXT string (also a dimension's label) shows.
/// Paragraph breaks (`\P`, `\X`, `\N`) become `\n`.
pub(crate) fn decode_mtext(raw: &str) -> String {
    decode(raw, true)
}

/// The characters a TEXT or ATTRIB string shows: only the `%%` codes and
/// `\U+XXXX` escapes apply; a backslash is otherwise a backslash.
pub(crate) fn decode_text(raw: &str) -> String {
    decode(raw, false)
}

fn decode(raw: &str, mtext: bool) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut plain = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            // \U+XXXX is the one backslash escape TEXT shares with MTEXT.
            if let Some((ch, len)) = unicode_escape(&chars[i..]) {
                plain.push(ch);
                i += len;
                continue;
            }
            if !mtext {
                plain.push(c);
                i += 1;
                continue;
            }
            let code = chars[i + 1];
            match code {
                'P' | 'X' | 'N' => plain.push('\n'),
                '~' => plain.push(' '),
                '\\' | '{' | '}' => plain.push(code),
                // Underline, overline and strike-through on and off: no
                // character of their own.
                'L' | 'l' | 'O' | 'o' | 'K' | 'k' => {}
                'S' => match chars[i + 2..].iter().position(|&ch| ch == ';') {
                    Some(p) => {
                        let body: String = chars[i + 2..i + 2 + p].iter().collect();
                        push_stack(&mut plain, &body);
                        i += 2 + p + 1;
                        continue;
                    }
                    // Unterminated: nothing sensible to do but keep it.
                    None => plain.push_str("\\S"),
                },
                // Format codes ended by `;`: alignment, colour, font,
                // height, slant, tracking, width, paragraph properties.
                'A' | 'C' | 'c' | 'f' | 'F' | 'H' | 'Q' | 'T' | 'W' | 'p' => {
                    if let Some(p) = chars[i + 2..].iter().position(|&ch| ch == ';') {
                        i += 2 + p + 1;
                        continue;
                    }
                    // No terminator: only the code letter is dropped.
                }
                // An unknown code, or a stray backslash in text nobody
                // escaped: the character is kept.
                other => plain.push(other),
            }
            i += 2;
            continue;
        }
        if mtext && (c == '{' || c == '}') {
            i += 1;
            continue;
        }
        if c == '%' && i + 2 < chars.len() && chars[i + 1] == '%' {
            if let Some((replacement, len)) = percent_code(&chars[i..]) {
                plain.push_str(&replacement);
                i += len;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    plain
}

/// `\U+XXXX` (four hex digits) at the start of `chars`, as the character and
/// the number of chars consumed.
fn unicode_escape(chars: &[char]) -> Option<(char, usize)> {
    if chars.len() < 7 || chars[0] != '\\' || chars[1] != 'U' || chars[2] != '+' {
        return None;
    }
    let hex: String = chars[3..7].iter().collect();
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let code = u32::from_str_radix(&hex, 16).ok()?;
    char::from_u32(code).map(|ch| (ch, 7))
}

/// The `%%` codes: `%%c` diameter, `%%d` degree, `%%p` plus-minus, `%%%` a
/// percent sign, `%%u`/`%%o` underline/overline toggles (nothing drawn),
/// `%%nnn` a character by code (Latin-1 for the upper half, matching the
/// Western code pages these codes were written for). A code below 32 other
/// than tab, line feed and carriage return is not a code: it could only
/// produce a character XML forbids. Returns the replacement and the number
/// of chars consumed, or `None` when the `%%` is not a code.
fn percent_code(chars: &[char]) -> Option<(String, usize)> {
    let code = chars.get(2)?;
    match code.to_ascii_lowercase() {
        'c' => Some(("\u{2205}".to_string(), 3)),
        'd' => Some(("\u{00B0}".to_string(), 3)),
        'p' => Some(("\u{00B1}".to_string(), 3)),
        '%' => Some(("%".to_string(), 3)),
        'u' | 'o' => Some((String::new(), 3)),
        _ if code.is_ascii_digit() => {
            let digits: String = chars[2..].iter().take(3).collect();
            if digits.len() != 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n: u32 = digits.parse().ok()?;
            let ch = if n < 256 {
                let ch = char::from_u32(n)?;
                if n < 32 && !matches!(ch, '\t' | '\n' | '\r') {
                    return None;
                }
                ch
            } else {
                '\u{FFFD}'
            };
            Some((ch.to_string(), 5))
        }
        _ => None,
    }
}

/// Writes the plain form of a `\S` stack body (`1#2`, `1/2`, `+0.1^-0.2`,
/// `2^`): `a/b`, or `a^b` for a tolerance stack, and one side alone for a
/// super- or subscript. A space goes in when a digit precedes it, so `3 1/2`
/// cannot read as thirty-one halves.
fn push_stack(plain: &mut String, body: &str) {
    let (tolerance, sep) = if let Some(p) = body.find('^') {
        (true, p)
    } else if let Some(p) = body.find('#').or_else(|| body.find('/')) {
        (false, p)
    } else {
        // No separator: AutoCAD shows the text unstacked.
        plain.push_str(body);
        return;
    };
    let numerator = body[..sep].trim();
    let denominator = body[sep + 1..].trim();
    if plain.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        plain.push(' ');
    }
    match (numerator.is_empty(), denominator.is_empty()) {
        (true, true) => {}
        (false, true) => plain.push_str(numerator),
        (true, false) => plain.push_str(denominator),
        (false, false) => {
            plain.push_str(numerator);
            plain.push(if tolerance { '^' } else { '/' });
            plain.push_str(denominator);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_labels_keep_their_numbers_readable() {
        assert_eq!(decode_mtext(r#"\A1;7'-4""#), r#"7'-4""#);
        assert_eq!(decode_mtext(r#"3{\H0.7x;\S1#2;}""#), r#"3 1/2""#);
        assert_eq!(decode_mtext(r#"\A1;{\H0.750000x;\S1#2;}""#), r#"1/2""#);
        assert_eq!(
            decode_mtext(r#"\A1;1'-1{\H0.750000x;\S1#8;}""#),
            r#"1'-1 1/8""#
        );
        assert_eq!(decode_mtext(r#"\A1;2"%%C"#), "2\"\u{2205}");
    }

    #[test]
    fn every_stack_separator_is_understood() {
        assert_eq!(decode_mtext(r"\S1/2;"), "1/2");
        assert_eq!(decode_mtext(r"\S1#2;"), "1/2");
        assert_eq!(decode_mtext(r"\S+0.1^-0.2;"), "+0.1^-0.2");
        // Super- and subscripts are one-sided stacks.
        assert_eq!(decode_mtext(r"m\S2^;"), "m2");
        assert_eq!(decode_mtext(r"H\S^2;"), "H2");
        assert_eq!(decode_mtext(r"\Sabc;"), "abc");
    }

    #[test]
    fn paragraphs_groups_and_format_codes() {
        assert_eq!(decode_mtext(r"A\PB\XC\ND"), "A\nB\nC\nD");
        assert_eq!(decode_mtext(r"\~x"), " x");
        assert_eq!(
            decode_mtext(r"\pi102.25;{\f@Arial Unicode MS|b1|i0|c0|p34;A T M O S}"),
            "A T M O S"
        );
        assert_eq!(decode_mtext(r"\C1;red\C256;bylayer"), "redbylayer");
        assert_eq!(decode_mtext(r"\fArial|b0|i0;\W0.8;\Q10;\T1.2;\H2.5;x"), "x");
        assert_eq!(decode_mtext(r"a\\b\{c\}"), r"a\b{c}");
        assert_eq!(
            decode_mtext(r"\Lunder\l \Oover\o \Kstrike\k"),
            "under over strike"
        );
        // A blank paragraph is a line of its own.
        assert_eq!(decode_mtext(r"ALPHA\P\PBRAVO"), "ALPHA\n\nBRAVO");
        // An unterminated format code drops only its letter.
        assert_eq!(decode_mtext(r"\Hoops"), "oops");
    }

    #[test]
    fn percent_codes_and_unicode_escapes() {
        assert_eq!(decode_text("%%c50"), "\u{2205}50");
        assert_eq!(decode_text("108%%d"), "108\u{00B0}");
        assert_eq!(decode_text("%%P0.5"), "\u{00B1}0.5");
        assert_eq!(decode_text("50%%%"), "50%");
        assert_eq!(decode_text("%%UBOOK RETURN%%U"), "BOOK RETURN");
        assert_eq!(decode_text("%%176"), "\u{00B0}");
        assert_eq!(decode_text("%%065"), "A");
        assert_eq!(decode_text(r"\U+00B1 3"), "\u{00B1} 3");
        assert_eq!(decode_mtext(r"\U+2205 50"), "\u{2205} 50");
        // Not codes: a lone percent, an unknown letter, a bad escape, and a
        // number that could only be a control character.
        assert_eq!(decode_text("50%"), "50%");
        assert_eq!(decode_text("a%%zb"), "a%%zb");
        assert_eq!(decode_text(r"\U+ZZZZ"), r"\U+ZZZZ");
        assert_eq!(decode_text("ZE%%001RO"), "ZE%%001RO");
        assert_eq!(decode_text("a%%009b"), "a\tb");
    }

    #[test]
    fn text_strings_keep_backslashes_and_braces() {
        assert_eq!(decode_text(r"C:\Temp\{x}"), r"C:\Temp\{x}");
        assert_eq!(
            decode_text(r"\P is not a paragraph here"),
            r"\P is not a paragraph here"
        );
        assert_eq!(decode_text(""), "");
        assert_eq!(decode_mtext("trailing\\"), "trailing\\");
    }

    #[test]
    fn what_the_code_stripping_it_replaces_got_right_still_holds() {
        assert_eq!(
            decode_mtext(r"\A1;Line1\PLine2 {\C1;colored} \S1#8;"),
            "Line1\nLine2 colored 1/8"
        );
        assert_eq!(decode_mtext(r"a\\b"), r"a\b");
        assert_eq!(decode_mtext(r"a\~b"), "a b");
    }

    #[test]
    fn korean_text_passes_through() {
        assert_eq!(
            decode_mtext("방 101\\P면적 32.5\u{33A1}"),
            "방 101\n면적 32.5\u{33A1}"
        );
    }
}
