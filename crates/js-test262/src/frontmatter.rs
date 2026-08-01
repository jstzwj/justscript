//! Test262 YAML frontmatter reader.
//!
//! The test262 frontmatter is a YAML block delimited by `/*---` ... `---*/`.
//! Unknown fields are intentionally ignored; malformed metadata is reported by
//! [`FrontMatter::parse_result`] instead of silently changing test semantics.

use serde::Deserialize;

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
        Self::parse_result(src).ok().flatten()
    }

    pub fn parse_result(src: &str) -> Result<Option<FrontMatter>, String> {
        let Some(start) = src.find("/*---") else {
            return Ok(None);
        };
        let after_start = &src[start + "/*---".len()..];
        let Some(end_rel) = after_start.find("---*/") else {
            return Err("unterminated test262 frontmatter".into());
        };
        let body = &after_start[..end_rel];
        let raw: RawFrontMatter = serde_yaml::from_str(body).map_err(|e| e.to_string())?;
        let (negative_phase, negative_type) = match raw.negative {
            Some(n) => (n.phase.map(NegativePhase::from), n.kind),
            None => (None, None),
        };
        Ok(Some(FrontMatter {
            flags: raw.flags,
            features: raw.features,
            includes: raw.includes,
            negative_phase,
            negative_type,
        }))
    }
}

#[derive(Default, Deserialize)]
struct RawFrontMatter {
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    includes: Vec<String>,
    negative: Option<RawNegative>,
}

#[derive(Deserialize)]
struct RawNegative {
    phase: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

impl From<String> for NegativePhase {
    fn from(value: String) -> Self {
        match value.as_str() {
            "parse" => NegativePhase::Parse,
            "early" => NegativePhase::Early,
            "resolution" => NegativePhase::Resolution,
            "runtime" => NegativePhase::Runtime,
            _ => NegativePhase::Other(value),
        }
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

    #[test]
    fn parses_multiline_lists() {
        let src =
            "/*---\nflags:\n  - onlyStrict\nincludes:\n  - assert.js\nfeatures:\n  - Symbol\n---*/";
        let fm = FrontMatter::parse_result(src).unwrap().unwrap();
        assert_eq!(fm.flags, ["onlyStrict"]);
        assert_eq!(fm.includes, ["assert.js"]);
        assert_eq!(fm.features, ["Symbol"]);
    }
}
