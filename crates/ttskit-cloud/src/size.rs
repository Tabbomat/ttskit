//! Human-readable byte sizes.

/// Formats `bytes` with decimal units: `999 B`, `1.5 kB`, `59.7 MB`, `99.9 GB`.
///
/// Sizes below 1000 are exact; larger ones have one decimal place.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    // Stop at the largest unit, and before rounding would show "1000.0".
    while (value >= 999.95) && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn small_sizes_are_exact() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(999), "999 B");
    }

    #[test]
    fn larger_sizes_use_decimal_units_with_one_decimal() {
        assert_eq!(format_size(1000), "1.0 kB");
        assert_eq!(format_size(1500), "1.5 kB");
        assert_eq!(format_size(59_727_444), "59.7 MB");
        assert_eq!(format_size(124_000_000), "124.0 MB");
        assert_eq!(format_size(99_900_000_000), "99.9 GB");
    }

    #[test]
    fn rounding_never_shows_1000() {
        assert_eq!(format_size(999_950), "1.0 MB");
        assert_eq!(format_size(999_949), "999.9 kB");
    }

    #[test]
    fn the_largest_value_does_not_overflow_the_units() {
        assert_eq!(format_size(u64::MAX), "18446.7 PB");
    }
}
