//! ECMAScript identifier properties pinned to Unicode 17.0.0.

include!("unicode_id_tables.rs");

pub(crate) fn is_id_start(ch: char) -> bool {
    in_ranges(ID_START_RANGES, ch as u32)
}

pub(crate) fn is_id_continue(ch: char) -> bool {
    in_ranges(ID_CONTINUE_RANGES, ch as u32)
}

fn in_ranges(ranges: &[(u32, u32)], code_point: u32) -> bool {
    ranges
        .binary_search_by(|&(start, end)| {
            if end < code_point {
                std::cmp::Ordering::Less
            } else if start > code_point {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_unicode_17_id_properties() {
        assert_eq!(UNICODE_VERSION, (17, 0, 0));
        for ch in ['\u{088f}', '\u{0c5c}', '\u{a7ce}', '\u{10940}'] {
            assert!(is_id_start(ch), "U+{:04X}", ch as u32);
        }
        for ch in ['\u{1acf}', '\u{10efa}', '\u{11b60}', '\u{1e6f5}'] {
            assert!(is_id_continue(ch), "U+{:04X}", ch as u32);
        }
    }

    #[test]
    fn includes_other_id_properties_without_using_xid() {
        for ch in [
            '\u{2118}', '\u{212e}', '\u{309b}', '\u{309c}', '\u{1885}', '\u{1886}',
        ] {
            assert!(is_id_start(ch), "U+{:04X}", ch as u32);
        }
        for ch in ['\u{00b7}', '\u{0387}', '\u{1369}', '\u{19da}'] {
            assert!(is_id_continue(ch), "U+{:04X}", ch as u32);
        }
        assert!(!is_id_start('0'));
        assert!(!is_id_start('😀'));
    }
}
