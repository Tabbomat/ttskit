//! Where the cards come from: directory expansion in natural order and card
//! list files.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::{IMAGE_EXTENSIONS, ImageError};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Chunk {
    /// A digit run without leading zeros: shorter is smaller, then by digits.
    Number(usize, String),
    Text(String),
}

fn chunks(text: &str) -> Vec<Chunk> {
    fn number(digits: &mut String, out: &mut Vec<Chunk>) {
        if !digits.is_empty() {
            let trimmed = digits.trim_start_matches('0').to_owned();
            out.push(Chunk::Number(trimmed.len(), trimmed));
            digits.clear();
        }
    }
    fn word(other: &mut String, out: &mut Vec<Chunk>) {
        if !other.is_empty() {
            out.push(Chunk::Text(std::mem::take(other)));
        }
    }
    let mut out = Vec::new();
    let mut digits = String::new();
    let mut other = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            word(&mut other, &mut out);
            digits.push(ch);
        } else {
            number(&mut digits, &mut out);
            other.extend(ch.to_lowercase());
        }
    }
    number(&mut digits, &mut out);
    word(&mut other, &mut out);
    out
}

/// Natural order of file names: digit runs compare by value (`card2` before
/// `card10`), the rest case-insensitively. Names that are equal that way are
/// ordered by the plain string, so the order is total and deterministic.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    chunks(a).cmp(&chunks(b)).then_with(|| a.cmp(b))
}

fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Expands card paths: a file stays where it is (the order is kept, a file may
/// repeat); a directory contributes its image files, not those of its
/// subdirectories, in natural order of their names.
pub fn expand_card_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, ImageError> {
    let mut cards = Vec::new();
    for input in inputs {
        if !input.is_dir() {
            cards.push(input.clone());
            continue;
        }
        let io = |source| ImageError::Io {
            path: input.clone(),
            source,
        };
        let mut names = Vec::new();
        for entry in std::fs::read_dir(input).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if !path.is_dir()
                && is_image_file(&path)
                && let Some(name) = path.file_name()
            {
                names.push(name.to_string_lossy().into_owned());
            }
        }
        names.sort_by(|a, b| natural_cmp(a, b));
        cards.extend(names.into_iter().map(|name| input.join(name)));
    }
    Ok(cards)
}

/// Reads a card list: one path per line, blank lines and lines starting with
/// `#` are ignored, a UTF-8 byte order mark is tolerated. A relative path is
/// relative to the directory of the list. The entries may be files or
/// directories; `expand_card_inputs` expands them.
pub fn read_card_list(list: &Path) -> Result<Vec<PathBuf>, ImageError> {
    let io = |source| ImageError::Io {
        path: list.to_owned(),
        source,
    };
    let bytes = std::fs::read(list).map_err(io)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| ImageError::Options(format!("{}: not UTF-8 text", list.display())))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let base = list.parent().unwrap_or(Path::new(""));
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let path = Path::new(line);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            }
        })
        .collect())
}
