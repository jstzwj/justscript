//! Numeric-literal well-formedness checks.
//!
//! The scanner in [`crate::lexer`] is deliberately permissive: it consumes the
//! maximal run that *could* be a number and stores the raw text. These
//! functions enforce the ES2024 12.8.3 / 12.8.4 rules that the scanner can't
//! express as a simple character class — chiefly numeric-separator placement
//! and BigInt restrictions. They are **unconditional** SyntaxErrors (independent
//! of strict mode).
//!
//! Strict-mode-only rules (legacy octal `077` in strict code) are intentionally
//! NOT enforced here — they need strict-mode tracking, which is a separate
//! milestone.

/// Why a numeric literal is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericError {
    /// A `_` separator appears where only a digit may flank it (leading,
    /// trailing, doubled, or next to `.`, radix letter, exponent sign, etc.).
    BadSeparator,
    /// A BigInt literal has a fractional or exponent part (`1.0n`, `1e0n`).
    BigIntFractional,
    /// A BigInt literal uses a legacy-octal-like leading zero (`08n`, `00n`).
    BigIntLeadingZero,
    /// The literal has no digits after a radix prefix / dot (`0x`, `1.`).
    Empty,
}

impl NumericError {
    pub fn message(self) -> &'static str {
        match self {
            NumericError::BadSeparator => "invalid numeric separator position",
            NumericError::BigIntFractional => "BigInt literal may not have a fractional or exponent part",
            NumericError::BigIntLeadingZero => "BigInt literal may not have a leading zero",
            NumericError::Empty => "numeric literal has no digits",
        }
    }
}

/// Validate the raw text of a `Numeric` or `Bigint` token. `raw` includes the
/// trailing `n` for BigInts (as produced by the scanner).
pub fn validate_numeric_literal(raw: &str) -> Result<(), NumericError> {
    let (is_bigint, body) = strip_bigint(raw);

    let (radix, digits) = match radix_prefix(body) {
        Some((r, d)) => (r, d),
        // No radix prefix → decimal.
        None => {
            return validate_decimal(body, is_bigint);
        }
    };

    // Radix-prefixed integer literal (0x.., 0b.., 0o..). These are always
    // integer literals — no fractional/exponent part exists, so a BigInt suffix
    // is always fine here. (Note: in `0x…E…`, 'E' is a hex digit, NOT an
    // exponent — the BigInt-fractional check must therefore run only in the
    // decimal branch, never against a hex body.)
    if digits.is_empty() {
        return Err(NumericError::Empty);
    }
    let _ = is_bigint;
    if !separators_ok(digits, digit_pred(radix)) {
        return Err(NumericError::BadSeparator);
    }
    Ok(())
}

/// Parse a `Numeric` token's raw text into an `f64`. Validates first, then
/// strips numeric separators (`_`) and dispatches by radix — Rust's own
/// `str::parse::<f64>()` neither accepts `_` nor hex/binary/octal literals, so
/// every numeric token must funnel through here. BigInt literals (trailing `n`)
/// are accepted and their integer value is returned lossily as `f64` (BigInt
/// execution is not supported; the raw text is retained on the AST).
pub fn parse_number(raw: &str) -> Result<f64, NumericError> {
    validate_numeric_literal(raw)?;
    let (_is_bigint, body) = strip_bigint(raw);

    if let Some((radix, digits)) = radix_prefix(body) {
        let cleaned = strip_underscores(digits);
        return u64::from_str_radix(&cleaned, radix)
            .map(|n| n as f64)
            .map_err(|_| NumericError::Empty);
    }

    // Decimal: drop separators and let Rust parse the rest (`.`, exponent, sign).
    let cleaned = strip_underscores(body);
    cleaned.parse::<f64>().map_err(|_| NumericError::Empty)
}

fn strip_bigint(raw: &str) -> (bool, &str) {
    if let Some(rest) = raw.strip_suffix('n') {
        (true, rest)
    } else {
        (false, raw)
    }
}

fn radix_prefix(s: &str) -> Option<(u32, &str)> {
    if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some((16, r))
    } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        Some((2, r))
    } else if let Some(r) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        Some((8, r))
    } else {
        None
    }
}

fn digit_pred(radix: u32) -> impl Fn(char) -> bool {
    move |c: char| c.is_digit(radix)
}

fn validate_decimal(body: &str, is_bigint: bool) -> Result<(), NumericError> {
    // DecimalIntegerLiteral [. DecimalDigits] [ExponentPart]
    // BigInt: only a plain DecimalIntegerLiteral is allowed — no '.', no
    // exponent. (This check is decimal-only: in a hex literal 'E' is a digit,
    // not an exponent, so the radix branch never runs it.)
    if is_bigint && (body.contains('.') || body.contains('e') || body.contains('E')) {
        return Err(NumericError::BigIntFractional);
    }
    // A non-zero decimal integer / bigint may not start with '0' unless it is
    // exactly "0" — but that legacy-octal rule is strict-mode-only for plain
    // numbers. For BigInt, leading-zero is unconditionally invalid.
    if is_bigint && has_leading_zero(body) {
        return Err(NumericError::BigIntLeadingZero);
    }

    // Split off exponent first: everything before e/E is the mantissa.
    let (mantissa, exponent) = match body.find(|c| c == 'e' || c == 'E') {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };

    // Mantissa: integer part [. fraction].
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };

    // At least one digit must be present somewhere in the mantissa.
    let int_digits = strip_underscores(int_part);
    let has_int = !int_digits.is_empty();
    let has_frac = frac_part.map_or(false, |f| !strip_underscores(f).is_empty());
    if !has_int && !has_frac {
        return Err(NumericError::Empty);
    }

    if !separators_ok(int_part, |c| c.is_ascii_digit()) {
        return Err(NumericError::BadSeparator);
    }
    if let Some(frac) = frac_part {
        if !separators_ok(frac, |c| c.is_ascii_digit()) {
            return Err(NumericError::BadSeparator);
        }
    }
    if let Some(exp) = exponent {
        // exponent := [+-] DecimalDigits
        let digits = exp.strip_prefix('+').or_else(|| exp.strip_prefix('-')).unwrap_or(exp);
        let digits = if digits.is_empty() {
            return Err(NumericError::Empty);
        } else {
            digits
        };
        if !separators_ok(digits, |c| c.is_ascii_digit()) {
            return Err(NumericError::BadSeparator);
        }
    }
    Ok(())
}

fn has_leading_zero(body: &str) -> bool {
    // Multi-digit decimal starting with '0' (e.g. "08", "00", "0_0"). A lone
    // "0" is fine.
    let chars: Vec<char> = body.chars().filter(|&c| c != '_').collect();
    chars.first() == Some(&'0') && chars.len() > 1
}

fn strip_underscores(s: &str) -> String {
    s.chars().filter(|&c| c != '_').collect()
}

/// Check that every `_` is flanked by a digit on both sides (per `is_digit`).
/// This rejects leading/trailing/doubled separators and separators adjacent to
/// non-digit characters.
fn separators_ok<F: Fn(char) -> bool>(s: &str, is_digit: F) -> bool {
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let next = chars.get(i + 1).copied();
            match (prev, next) {
                (Some(p), Some(n)) if is_digit(p) && is_digit(n) => {}
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed() {
        assert!(validate_numeric_literal("1").is_ok());
        assert!(validate_numeric_literal("1_000").is_ok());
        assert!(validate_numeric_literal("1.5_5").is_ok());
        assert!(validate_numeric_literal("1.5e1_0").is_ok());
        assert!(validate_numeric_literal("0xff_ff").is_ok());
        assert!(validate_numeric_literal("0b1010_0011").is_ok());
        assert!(validate_numeric_literal("0n").is_ok());
        assert!(validate_numeric_literal("1_000n").is_ok());
    }

    #[test]
    fn rejects_bad_separators() {
        assert_eq!(validate_numeric_literal("_1"), Err(NumericError::BadSeparator));
        assert_eq!(validate_numeric_literal("1_"), Err(NumericError::BadSeparator));
        assert_eq!(validate_numeric_literal("1__2"), Err(NumericError::BadSeparator));
        assert_eq!(validate_numeric_literal("0x_1"), Err(NumericError::BadSeparator));
        assert_eq!(validate_numeric_literal("1_e5"), Err(NumericError::BadSeparator));
        assert_eq!(validate_numeric_literal("1.0_"), Err(NumericError::BadSeparator));
    }

    #[test]
    fn rejects_bad_bigint() {
        assert_eq!(validate_numeric_literal("1.0n"), Err(NumericError::BigIntFractional));
        assert_eq!(validate_numeric_literal("1e0n"), Err(NumericError::BigIntFractional));
        assert_eq!(validate_numeric_literal("08n"), Err(NumericError::BigIntLeadingZero));
        assert_eq!(validate_numeric_literal("00n"), Err(NumericError::BigIntLeadingZero));
        assert_eq!(validate_numeric_literal("0_0n"), Err(NumericError::BigIntLeadingZero));
    }

    #[test]
    fn accepts_radix_bigint_with_hex_letter_e() {
        // `0x…E…`: 'E' is a hex digit, not an exponent — must NOT trip the
        // BigInt-fractional rule.
        assert!(validate_numeric_literal("0xFEDCBA9876543210n").is_ok());
        assert!(validate_numeric_literal("0xDEAD_BEEFn").is_ok());
        assert!(validate_numeric_literal("0b1010n").is_ok());
        assert!(validate_numeric_literal("0o777n").is_ok());
    }

    #[test]
    fn parses_separated_and_radix() {
        assert_eq!(parse_number("1_000"), Ok(1000.0));
        assert_eq!(parse_number("1.5_5"), Ok(1.55));
        assert_eq!(parse_number("1.5e1_0"), Ok(1.5e10));
        assert_eq!(parse_number("0xff_ff"), Ok(65535.0));
        assert_eq!(parse_number("0b1010_0011"), Ok(163.0));
        assert_eq!(parse_number("0o17"), Ok(15.0));
        assert_eq!(parse_number("1_000n"), Ok(1000.0));
        assert!(parse_number("1_").is_err());
        assert!(parse_number("0x_1").is_err());
    }
}
