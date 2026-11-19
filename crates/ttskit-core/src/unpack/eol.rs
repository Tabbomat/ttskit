//! Line ending detection and conversion for extracted text fields.

use crate::save::write::Eol;

/// Counts of line breaks in a text. A lone `\r` is not a line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Breaks {
    pub crlf: usize,
    pub lf: usize,
}

/// The recorded style of a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub eol: Eol,
    pub mixed: bool,
}

/// Counts `\r\n` and lone `\n` line breaks.
pub fn count_breaks(s: &str) -> Breaks {
    let bytes = s.as_bytes();
    let mut counts = Breaks::default();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                counts.crlf += 1;
            } else {
                counts.lf += 1;
            }
        }
    }
    counts
}

/// The style of a text: `None` without line breaks. Mixed texts get the
/// majority style, a tie counts as LF.
pub fn detect(s: &str) -> Option<Style> {
    let Breaks { crlf, lf } = count_breaks(s);
    match (crlf, lf) {
        (0, 0) => None,
        (_, 0) => Some(Style {
            eol: Eol::Crlf,
            mixed: false,
        }),
        (0, _) => Some(Style {
            eol: Eol::Lf,
            mixed: false,
        }),
        _ => Some(Style {
            eol: if crlf > lf { Eol::Crlf } else { Eol::Lf },
            mixed: true,
        }),
    }
}

/// Converts every line break (`\r\n` or `\n`) to `eol`. A lone `\r` and all
/// other bytes are kept.
pub fn convert(s: &str, eol: Eol) -> Vec<u8> {
    let nl = eol.as_bytes();
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 8);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                out.extend_from_slice(nl);
                i += 2;
            }
            b'\n' => {
                out.extend_from_slice(nl);
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The line ending style of raw input bytes: CRLF if it contains `\r\n`, else
/// LF if it contains `\n`, else `None`.
pub fn detect_layout(bytes: &[u8]) -> Option<Eol> {
    if bytes.windows(2).any(|w| w == b"\r\n") {
        Some(Eol::Crlf)
    } else if bytes.contains(&b'\n') {
        Some(Eol::Lf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(s: &str) -> Option<(Eol, bool)> {
        detect(s).map(|d| (d.eol, d.mixed))
    }

    #[test]
    fn detection() {
        assert_eq!(style("no breaks"), None);
        assert_eq!(style(""), None);
        assert_eq!(style("a\r\nb\r\n"), Some((Eol::Crlf, false)));
        assert_eq!(style("a\nb"), Some((Eol::Lf, false)));
        assert_eq!(style("a\r\nb\r\nc\nd"), Some((Eol::Crlf, true)));
        assert_eq!(style("a\nb\nc\r\nd"), Some((Eol::Lf, true)));
        // Tie goes to LF.
        assert_eq!(style("a\r\nb\nc"), Some((Eol::Lf, true)));
    }

    #[test]
    fn lone_cr_is_not_a_line_break() {
        assert_eq!(style("a\rb"), None);
        assert_eq!(style("a\rb\r\nc"), Some((Eol::Crlf, false)));
        assert_eq!(count_breaks("a\r\r\nb\rc\n"), Breaks { crlf: 1, lf: 1 });
    }

    #[test]
    fn conversion() {
        assert_eq!(convert("a\r\nb\nc", Eol::Lf), b"a\nb\nc");
        assert_eq!(convert("a\r\nb\nc", Eol::Crlf), b"a\r\nb\r\nc");
        assert_eq!(convert("a\rb\r\r\nc", Eol::Lf), b"a\rb\r\nc");
        assert_eq!(convert("a\rb\r\r\nc", Eol::Crlf), b"a\rb\r\r\nc");
        assert_eq!(convert("x\n", Eol::Crlf), b"x\r\n");
        assert_eq!(convert("ö日\n", Eol::Lf), "ö日\n".as_bytes());
        assert_eq!(convert("", Eol::Crlf), b"");
    }

    #[test]
    fn layout_detection() {
        assert_eq!(detect_layout(b"{\r\n}"), Some(Eol::Crlf));
        assert_eq!(detect_layout(b"{\n}"), Some(Eol::Lf));
        assert_eq!(detect_layout(b"{}"), None);
        assert_eq!(detect_layout(b"{\n\r\n}"), Some(Eol::Crlf));
    }
}
