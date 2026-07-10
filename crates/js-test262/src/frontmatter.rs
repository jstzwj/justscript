//! Minimal test262 frontmatter reader.
//!
//! The test262 frontmatter is a YAML block delimited by `/*---` ... `---*/`.
//! We don't pull in a YAML crate; we parse just the fields the runner needs.

/// The phase of a `negative` test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NegativePhase {
    Parse,
    Early,
    Resolution,
    Runtime,
    Other(String),
}

#[derive(Default, Clone, Debug)]
pub struct FrontMatter {
    pub flags: Vec<String>,
    pub features: Vec<String>,
    pub includes: Vec<String>,
    pub negative_phase: Option<NegativePhase>,
    pub negative_type: Option<String>,
}

impl FrontMatter {
    /// Parse the frontmatter out of a test file's source. Returns `None` if no
    /// frontmatter block is present.
    pub fn parse(src: &str) -> Option<FrontMatter> {
        let start = src.find("/*---")?;
        let after_start = &src[start + "/*---".len()..];
        let end_rel = after_start.find("---*/")?;
        let body = &after_start[..end_rel];

        let mut fm = FrontMatter::default();
        let mut in_negative = false;
        for raw_line in body.lines() {
            let line = raw_line.trim_end();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Indentation determines whether we're inside a nested block.
            let indented = line.starts_with(' ') || line.starts_with('\t');

            if !indented {
                in_negative = false;
            }

            let (key, rest) = match trimmed.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };

            match key {
                "flags" | "features" | "includes" => {
                    let items = parse_list(rest);
                    match key {
                        "flags" => fm.flags = items,
                        "features" => fm.features = items,
                        "includes" => fm.includes = items,
                        _ => {}
                    }
                }
                "negative" => {
                    in_negative = true;
                }
                "phase" if in_negative => {
                    fm.negative_phase = Some(match rest {
                        "parse" => NegativePhase::Parse,
                        "early" => NegativePhase::Early,
                        "resolution" => NegativePhase::Resolution,
                        "runtime" => NegativePhase::Runtime,
                        other => NegativePhase::Other(other.to_string()),
                    });
                }
                "type" if in_negative => {
                    fm.negative_type = Some(strip_quotes(rest));
                }
                _ => {}
            }
        }
        Some(fm)
    }
}

fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    s.split(',')
        .map(|p| strip_quotes(p.trim()))
        .filter(|p| !p.is_empty())
        .collect()
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_negative() {
        let src = "/*---\ndescription: x\nflags: [module]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\ncode";
        let fm = FrontMatter::parse(src).unwrap();
        assert_eq!(fm.flags, vec!["module".to_string()]);
        assert_eq!(fm.negative_phase, Some(NegativePhase::Parse));
        assert_eq!(fm.negative_type.as_deref(), Some("SyntaxError"));
    }
}
