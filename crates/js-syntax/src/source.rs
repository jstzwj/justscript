//! Source positions and spans.
//!
//! Modeled after `rustc_span`: every offset is a [`BytePos`] (a byte offset
//! into a [`SourceFile`]), and a [`Span`] is a half-open byte range that can be
//! resolved back to a line/column [`Loc`] for diagnostics.

use std::path::Path;
use std::sync::Arc;

/// A byte offset into a source file.
///
/// Wrapping the raw `u32` in a newtype prevents accidentally mixing byte
/// offsets with character indices or user-facing numbers.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct BytePos(pub u32);

impl BytePos {
    pub const ZERO: BytePos = BytePos(0);

    #[inline]
    pub fn to_u32(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl std::ops::Add<u32> for BytePos {
    type Output = BytePos;
    #[inline]
    fn add(self, rhs: u32) -> BytePos {
        BytePos(self.0 + rhs)
    }
}

impl std::ops::Sub<u32> for BytePos {
    type Output = BytePos;
    #[inline]
    fn sub(self, rhs: u32) -> BytePos {
        BytePos(self.0 - rhs)
    }
}

/// A half-open byte range `[start, end)` within a [`SourceFile`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
}

impl Span {
    /// The empty / "phantom" span — used when no real location is available.
    pub const DUMMY: Span = Span {
        start: BytePos(0),
        end: BytePos(0),
    };

    #[inline]
    pub fn new(start: BytePos, end: BytePos) -> Span {
        Span { start, end }
    }

    #[inline]
    pub fn is_dummy(self) -> bool {
        self == Span::DUMMY
    }

    /// Byte length covered by this span.
    #[inline]
    pub fn len(self) -> u32 {
        self.end.0.saturating_sub(self.start.0)
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        let start = self.start.min(other.start);
        let end = self.end.max(other.end);
        Span { start, end }
    }

    /// Extract the source text covered by this span, if available.
    pub fn snippet<'a>(&self, src: &'a str) -> Option<&'a str> {
        let start = self.start.to_usize();
        let end = self.end.to_usize();
        src.get(start..end)
    }
}

/// A 1-based line/column location.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct Loc {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column (in bytes) within the line.
    pub column: u32,
}

impl Loc {
    pub fn new(line: u32, column: u32) -> Loc {
        Loc { line, column }
    }
}

/// An owned snapshot of a source file.
///
/// Cheaply shared via [`Arc`]; lexers and parsers hold a clone of the handle
/// and resolve [`Span`]s against it for diagnostics.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// File name as given (e.g. `"repl"` or a real path).
    pub name: String,
    /// The full source text.
    pub src: Arc<str>,
    /// Byte offset of each *line start* (line N starts at `line_starts[N-1]`).
    /// Always begins with `BytePos(0)` for line 1.
    pub line_starts: Vec<BytePos>,
}

impl SourceFile {
    /// Build a source file from a name + text, precomputing line starts.
    pub fn new(name: impl Into<String>, src: impl Into<Arc<str>>) -> SourceFile {
        let src = src.into();
        let mut line_starts = vec![BytePos::ZERO];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(BytePos((i + 1) as u32));
            }
        }
        SourceFile {
            name: name.into(),
            src,
            line_starts,
        }
    }

    pub fn from_path(path: &Path, src: impl Into<Arc<str>>) -> SourceFile {
        let name = path.display().to_string();
        SourceFile::new(name, src)
    }

    /// Resolve a [`BytePos`] into a 1-based [`Loc`].
    pub fn loc(&self, pos: BytePos) -> Loc {
        let needle = pos.0;
        // Binary search for the last line_start <= needle.
        let line_idx = match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[line_idx].0;
        Loc {
            line: (line_idx as u32) + 1,
            column: needle.saturating_sub(line_start) + 1,
        }
    }

    /// Resolve a full [`Span`] into `(start_loc, end_loc)`.
    pub fn span_locs(&self, span: Span) -> (Loc, Loc) {
        (self.loc(span.start), self.loc(span.end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loc_basic() {
        let sf = SourceFile::new("t", std::sync::Arc::<str>::from("ab\ncd\n"));
        assert_eq!(sf.loc(BytePos(0)), Loc::new(1, 1));
        assert_eq!(sf.loc(BytePos(2)), Loc::new(1, 3));
        assert_eq!(sf.loc(BytePos(3)), Loc::new(2, 1));
        assert_eq!(sf.loc(BytePos(5)), Loc::new(2, 3));
    }

    #[test]
    fn span_snippet() {
        let src = "hello world";
        let span = Span::new(BytePos(6), BytePos(11));
        assert_eq!(span.snippet(src), Some("world"));
    }
}
