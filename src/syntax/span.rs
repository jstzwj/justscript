use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Add, Sub};

// https://github.com/rust-lang/rust/blob/master/src/librustc_span/lib.rs
pub trait Pos {
    fn from_usize(n: usize) -> Self;
    fn to_usize(&self) -> usize;
    fn from_u32(n: u32) -> Self;
    fn to_u32(&self) -> u32;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BytePos(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct CharPos(pub usize);


impl Pos for BytePos {
    #[inline(always)]
    fn from_usize(n: usize) -> BytePos {
        BytePos(n as u32)
    }

    #[inline(always)]
    fn to_usize(&self) -> usize {
        self.0 as usize
    }

    #[inline(always)]
    fn from_u32(n: u32) -> BytePos {
        BytePos(n)
    }

    #[inline(always)]
    fn to_u32(&self) -> u32 {
        self.0
    }
}

impl Add for BytePos {
    type Output = BytePos;

    #[inline(always)]
    fn add(self, rhs: BytePos) -> BytePos {
        BytePos((self.to_usize() + rhs.to_usize()) as u32)
    }
}

impl Sub for BytePos {
    type Output = BytePos;

    #[inline(always)]
    fn sub(self, rhs: BytePos) -> BytePos {
        BytePos((self.to_usize() - rhs.to_usize()) as u32)
    }
}

impl Pos for CharPos {
    #[inline(always)]
    fn from_usize(n: usize) -> CharPos {
        CharPos(n)
    }

    #[inline(always)]
    fn to_usize(&self) -> usize {
        self.0
    }

    #[inline(always)]
    fn from_u32(n: u32) -> CharPos {
        CharPos(n as usize)
    }

    #[inline(always)]
    fn to_u32(&self) -> u32 {
        self.0 as u32
    }
}

impl Add for CharPos {
    type Output = CharPos;

    #[inline(always)]
    fn add(self, rhs: CharPos) -> CharPos {
        CharPos(self.to_usize() + rhs.to_usize())
    }
}

impl Sub for CharPos {
    type Output = CharPos;

    #[inline(always)]
    fn sub(self, rhs: CharPos) -> CharPos {
        CharPos(self.to_usize() - rhs.to_usize())
    }
}

pub struct SourceFile {
    pub start_pos: BytePos,
    pub src: Option<String>
}

impl SourceFile {
    pub fn new(code:&str) -> SourceFile{
        SourceFile {
            start_pos: BytePos(0),
            src: Some(String::from(code))
        }
    }
}

#[derive(Debug)]
pub struct Span {
   begin: BytePos,
   end: BytePos
}

impl Span {
    pub fn new(b:BytePos, e:BytePos) -> Span{
        Span {
            begin: b,
            end: e,
        }
    }

    pub fn make_span(b:usize, e:usize) -> Span {
        Span {
            begin: BytePos::from_usize(b),
            end: BytePos::from_usize(e),
        }
    }
}

// used for diagnostic
#[derive(Debug)]
pub struct Loc {
    pub column_number: u64,
    pub line_number: u64,
}

impl Loc {
    pub fn new(line_number: u64, column_number: u64) -> Self {
        Self {
            line_number,
            column_number,
        }
    }
}
