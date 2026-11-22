//! Human-readable byte sizes.

/// Formats `bytes` with binary units, as the in-game cloud manager does:
/// `999 B`, `1.5 KiB`, `56.6 MiB`, `93.0 GiB`.
///
/// Sizes below 1024 are exact; larger ones have one decimal place.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    // Stop at the largest unit, and before rounding would show "1024.0".
    while value >= 1023.95 && unit + 1 < UNITS.len() {
        value /= 1024.0;
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
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn larger_sizes_use_binary_units_with_one_decimal() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(118_786_379), "113.3 MiB");
        assert_eq!(format_size(99_881_213_621), "93.0 GiB");
        assert_eq!(format_size(100_000_000_000), "93.1 GiB");
    }

    #[test]
    fn rounding_never_shows_1024() {
        assert_eq!(format_size(1024 * 1024 - 1), "1.0 MiB");
        assert_eq!(format_size(1024 * 1024 - 52), "1023.9 KiB");
    }

    #[test]
    fn the_largest_value_does_not_overflow_the_units() {
        assert_eq!(format_size(u64::MAX), "16.0 EiB");
    }
}
