//! Value parsers shared with the `ttskit image` command line.

/// Parses `MIN-MAX`, each 0 to 100.
pub fn parse_quality(text: &str) -> Result<(u8, u8), String> {
    let bad = || format!("`{text}` is not a quality range; use MIN-MAX, e.g. 60-90");
    let (min, max) = text.split_once('-').ok_or_else(bad)?;
    let min: u8 = min.trim().parse().map_err(|_| bad())?;
    let max: u8 = max.trim().parse().map_err(|_| bad())?;
    if min > 100 || max > 100 {
        return Err(format!("quality values must be 0 to 100, not `{text}`"));
    }
    if min > max {
        return Err(format!("quality minimum is above the maximum in `{text}`"));
    }
    Ok((min, max))
}

/// Parses a dithering strength, 0.0 to 1.0.
pub fn parse_dither(text: &str) -> Result<f32, String> {
    let value: f32 = text
        .parse()
        .map_err(|_| format!("`{text}` is not a number"))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(format!("dither must be between 0 and 1, not `{text}`"));
    }
    Ok(value)
}

/// Parses a resize scale: a positive number.
pub fn parse_scale(text: &str) -> Result<f64, String> {
    match text.trim().parse::<f64>() {
        Ok(f) if f.is_finite() && f > 0.0 => Ok(f),
        _ => Err(format!(
            "invalid scale '{text}': use a positive number, e.g. 0.5"
        )),
    }
}

/// Parses a card size: `WIDTHxHEIGHT`, both at least 1.
pub fn parse_card_size(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("invalid card size '{text}': use WIDTHxHEIGHT in pixels, e.g. 300x420");
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    match (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
        (Ok(w), Ok(h)) if w >= 1 && h >= 1 => Ok((w, h)),
        _ => Err(bad()),
    }
}
