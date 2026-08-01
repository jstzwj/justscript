//! Static validation for the ECMAScript RegExp Pattern grammar.
//!
//! The lexer owns literal boundaries (`/.../flags`). Pattern grammar is an
//! early error, so it is checked here after both the body and flags are known.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupKind {
    Atom,
    LookAhead,
    Assertion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClassAtom {
    Singleton(char),
    Set,
}

pub(crate) fn validate_pattern(pattern: &str, flags: &str) -> Result<(), &'static str> {
    let chars: Vec<char> = pattern.chars().collect();
    let unicode = flags.contains('u');
    let captures = capture_count(&chars);
    let mut groups = Vec::new();
    let mut pos = 0;
    let mut quantifiable = None;
    let mut just_quantified = false;

    while pos < chars.len() {
        match chars[pos] {
            '\\' => {
                let (end, assertion) = parse_escape(&chars, pos, unicode, false, captures)?;
                pos = end;
                quantifiable = Some(if assertion {
                    GroupKind::Assertion
                } else {
                    GroupKind::Atom
                });
                just_quantified = false;
            }
            '[' => {
                pos = parse_class(&chars, pos, unicode, captures)?;
                quantifiable = Some(GroupKind::Atom);
                just_quantified = false;
            }
            '(' => {
                let (kind, prefix_len) = group_prefix(&chars, pos)?;
                groups.push(kind);
                pos += prefix_len;
                quantifiable = None;
                just_quantified = false;
            }
            ')' => {
                let Some(kind) = groups.pop() else {
                    return Err("unmatched `)` in regular expression");
                };
                pos += 1;
                quantifiable = Some(kind);
                just_quantified = false;
            }
            '^' | '$' => {
                pos += 1;
                quantifiable = Some(GroupKind::Assertion);
                just_quantified = false;
            }
            '|' => {
                pos += 1;
                quantifiable = None;
                just_quantified = false;
            }
            '*' | '+' | '?' => {
                if chars[pos] == '?' && just_quantified {
                    pos += 1;
                    just_quantified = false;
                    continue;
                }
                require_quantifiable(quantifiable, unicode)?;
                pos += 1;
                quantifiable = None;
                just_quantified = true;
            }
            '{' => {
                if let Some((end, min, max)) = braced_quantifier(&chars, pos) {
                    require_quantifiable(quantifiable, unicode)?;
                    if max.is_some_and(|max| max < min) {
                        return Err("regular expression quantifier range is out of order");
                    }
                    pos = end;
                    quantifiable = None;
                    just_quantified = true;
                } else if unicode {
                    return Err("unescaped `{` is not allowed in a Unicode pattern");
                } else {
                    pos += 1;
                    quantifiable = Some(GroupKind::Atom);
                    just_quantified = false;
                }
            }
            ']' | '}' if unicode => {
                return Err("unescaped regular expression syntax character");
            }
            '.' => {
                pos += 1;
                quantifiable = Some(GroupKind::Atom);
                just_quantified = false;
            }
            _ => {
                pos += 1;
                quantifiable = Some(GroupKind::Atom);
                just_quantified = false;
            }
        }
    }

    if groups.is_empty() {
        Ok(())
    } else {
        Err("unterminated group in regular expression")
    }
}

fn require_quantifiable(kind: Option<GroupKind>, unicode: bool) -> Result<(), &'static str> {
    match kind {
        Some(GroupKind::Atom) => Ok(()),
        // Annex B permits quantified lookaheads in non-Unicode patterns.
        Some(GroupKind::LookAhead) if !unicode => Ok(()),
        _ => Err("regular expression quantifier has no valid atom"),
    }
}

fn group_prefix(chars: &[char], pos: usize) -> Result<(GroupKind, usize), &'static str> {
    if chars.get(pos + 1) != Some(&'?') {
        return Ok((GroupKind::Atom, 1));
    }
    match (chars.get(pos + 2), chars.get(pos + 3)) {
        (Some(':'), _) => Ok((GroupKind::Atom, 3)),
        (Some('=' | '!'), _) => Ok((GroupKind::LookAhead, 3)),
        (Some('<'), Some('=' | '!')) => Ok((GroupKind::Assertion, 4)),
        (Some('<'), _) => {
            let mut end = pos + 3;
            while end < chars.len() && chars[end] != '>' {
                end += 1;
            }
            if end == pos + 3 || end == chars.len() {
                Err("invalid named capture group")
            } else {
                Ok((GroupKind::Atom, end - pos + 1))
            }
        }
        _ => Err("invalid regular expression group prefix"),
    }
}

fn braced_quantifier(chars: &[char], pos: usize) -> Option<(usize, usize, Option<usize>)> {
    let mut cursor = pos + 1;
    let min_start = cursor;
    while chars.get(cursor).is_some_and(char::is_ascii_digit) {
        cursor += 1;
    }
    if cursor == min_start {
        return None;
    }
    let min = decimal(chars, min_start, cursor)?;
    let max = if chars.get(cursor) == Some(&',') {
        cursor += 1;
        let max_start = cursor;
        while chars.get(cursor).is_some_and(char::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == max_start {
            None
        } else {
            Some(decimal(chars, max_start, cursor)?)
        }
    } else {
        Some(min)
    };
    (chars.get(cursor) == Some(&'}')).then_some((cursor + 1, min, max))
}

fn decimal(chars: &[char], start: usize, end: usize) -> Option<usize> {
    chars[start..end].iter().try_fold(0usize, |value, digit| {
        value
            .checked_mul(10)?
            .checked_add(digit.to_digit(10)? as usize)
    })
}

fn parse_class(
    chars: &[char],
    start: usize,
    unicode: bool,
    captures: usize,
) -> Result<usize, &'static str> {
    let mut pos = start + 1;
    if chars.get(pos) == Some(&'^') {
        pos += 1;
    }
    while pos < chars.len() {
        if chars[pos] == ']' {
            return Ok(pos + 1);
        }
        let (next, left) = parse_class_atom(chars, pos, unicode, captures)?;
        pos = next;
        if chars.get(pos) == Some(&'-') && chars.get(pos + 1) != Some(&']') {
            let (next, right) = parse_class_atom(chars, pos + 1, unicode, captures)?;
            if unicode {
                match (left, right) {
                    (ClassAtom::Singleton(a), ClassAtom::Singleton(b)) if a <= b => {}
                    (ClassAtom::Singleton(_), ClassAtom::Singleton(_)) => {
                        return Err("regular expression character class range is out of order");
                    }
                    _ => return Err("character class escape cannot be a range endpoint"),
                }
            }
            pos = next;
        }
    }
    Err("unterminated regular expression character class")
}

fn parse_class_atom(
    chars: &[char],
    pos: usize,
    unicode: bool,
    captures: usize,
) -> Result<(usize, ClassAtom), &'static str> {
    if chars.get(pos) == Some(&'\\') {
        let (end, kind) = parse_escape_kind(chars, pos, unicode, true, captures)?;
        Ok((end, kind))
    } else {
        chars
            .get(pos)
            .copied()
            .map(|character| (pos + 1, ClassAtom::Singleton(character)))
            .ok_or("unterminated regular expression character class")
    }
}

fn parse_escape(
    chars: &[char],
    pos: usize,
    unicode: bool,
    in_class: bool,
    captures: usize,
) -> Result<(usize, bool), &'static str> {
    let (end, kind) = parse_escape_kind(chars, pos, unicode, in_class, captures)?;
    let assertion = !in_class
        && matches!(chars.get(pos + 1), Some('b' | 'B'))
        && matches!(kind, ClassAtom::Set);
    Ok((end, assertion))
}

fn parse_escape_kind(
    chars: &[char],
    pos: usize,
    unicode: bool,
    in_class: bool,
    captures: usize,
) -> Result<(usize, ClassAtom), &'static str> {
    let Some(&escaped) = chars.get(pos + 1) else {
        return Err("incomplete regular expression escape");
    };
    let mut end = pos + 2;
    let result = match escaped {
        'd' | 'D' | 's' | 'S' | 'w' | 'W' => ClassAtom::Set,
        'b' if !in_class => ClassAtom::Set,
        'B' if !in_class => ClassAtom::Set,
        'b' | 'f' | 'n' | 'r' | 't' | 'v' => ClassAtom::Singleton(escaped),
        'c' => {
            let Some(&control) = chars.get(end) else {
                return Err("incomplete regular expression control escape");
            };
            if !control.is_ascii_alphabetic() {
                if unicode {
                    return Err("invalid regular expression control escape");
                }
            } else {
                end += 1;
            }
            ClassAtom::Singleton(control)
        }
        'x' if unicode => {
            end = fixed_hex_escape(chars, end, 2)?;
            ClassAtom::Singleton('\0')
        }
        'u' if unicode => {
            end = unicode_escape(chars, end)?;
            ClassAtom::Singleton('\0')
        }
        '0' => {
            if unicode && chars.get(end).is_some_and(char::is_ascii_digit) {
                return Err("legacy octal escape is not allowed in a Unicode pattern");
            }
            ClassAtom::Singleton('\0')
        }
        '1'..='9' if unicode => {
            if in_class {
                return Err("decimal escape is not allowed in a Unicode character class");
            }
            let start = pos + 1;
            while chars.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
            if decimal(chars, start, end).is_none_or(|index| index > captures) {
                return Err("invalid decimal backreference in Unicode pattern");
            }
            ClassAtom::Singleton('\0')
        }
        'k' if unicode => {
            if chars.get(end) != Some(&'<') {
                return Err("invalid named backreference in Unicode pattern");
            }
            end += 1;
            let name_start = end;
            while end < chars.len() && chars[end] != '>' {
                end += 1;
            }
            if end == name_start || end == chars.len() {
                return Err("invalid named backreference in Unicode pattern");
            }
            end += 1;
            ClassAtom::Singleton('\0')
        }
        'p' | 'P' if unicode => {
            if chars.get(end) != Some(&'{') {
                return Err("invalid Unicode property escape");
            }
            end += 1;
            let property_start = end;
            while end < chars.len() && chars[end] != '}' {
                end += 1;
            }
            if end == property_start || end == chars.len() {
                return Err("invalid Unicode property escape");
            }
            end += 1;
            ClassAtom::Set
        }
        _ if unicode => {
            let syntax = "^$\\.*+?()[]{}|/".contains(escaped) || (in_class && escaped == '-');
            if !syntax {
                return Err("invalid identity escape in Unicode pattern");
            }
            ClassAtom::Singleton(escaped)
        }
        _ => ClassAtom::Singleton(escaped),
    };
    Ok((end, result))
}

fn fixed_hex_escape(chars: &[char], start: usize, digits: usize) -> Result<usize, &'static str> {
    let end = start + digits;
    if end <= chars.len() && chars[start..end].iter().all(char::is_ascii_hexdigit) {
        Ok(end)
    } else {
        Err("invalid hexadecimal escape in Unicode pattern")
    }
}

fn unicode_escape(chars: &[char], start: usize) -> Result<usize, &'static str> {
    if chars.get(start) != Some(&'{') {
        return fixed_hex_escape(chars, start, 4);
    }
    let mut end = start + 1;
    let digit_start = end;
    let mut value = 0u32;
    while let Some(digit) = chars.get(end).and_then(|character| character.to_digit(16)) {
        value = value
            .checked_mul(16)
            .and_then(|current| current.checked_add(digit))
            .ok_or("Unicode escape is out of range")?;
        end += 1;
    }
    if end == digit_start || chars.get(end) != Some(&'}') || value > 0x10ffff {
        Err("invalid Unicode code point escape")
    } else {
        Ok(end + 1)
    }
}

fn capture_count(chars: &[char]) -> usize {
    let mut count = 0;
    let mut pos = 0;
    let mut in_class = false;
    while pos < chars.len() {
        match chars[pos] {
            '\\' => pos += 2,
            '[' => {
                in_class = true;
                pos += 1;
            }
            ']' => {
                in_class = false;
                pos += 1;
            }
            '(' if !in_class => {
                if chars.get(pos + 1) != Some(&'?')
                    || matches!(chars.get(pos + 2), Some('<'))
                        && !matches!(chars.get(pos + 3), Some('=' | '!'))
                {
                    count += 1;
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::validate_pattern;

    #[test]
    fn validates_quantifier_and_assertion_positions() {
        for pattern in ["?", "{2}", "{2,}", "{2,3}", ".(?<=.)?", ".(?<!.){2,3}"] {
            assert!(validate_pattern(pattern, "").is_err(), "{pattern}");
        }
        assert!(validate_pattern(".(?=.)?", "").is_ok());
        assert!(validate_pattern(".(?=.)?", "u").is_err());
    }

    #[test]
    fn validates_unicode_escapes_and_class_ranges() {
        for pattern in [
            "\\c0",
            "\\M",
            "\\1",
            "\\8",
            "\\k",
            "\\u{110000}",
            "\\u{1,}",
            "[\\d-a]",
            "[\\s-\\d]",
            "[%-\\d]",
        ] {
            assert!(validate_pattern(pattern, "u").is_err(), "{pattern}");
        }
        for pattern in ["\\cA", "\\u{10ffff}", "[a-z]", "(?<a>a)\\1"] {
            assert!(validate_pattern(pattern, "u").is_ok(), "{pattern}");
        }
    }
}
