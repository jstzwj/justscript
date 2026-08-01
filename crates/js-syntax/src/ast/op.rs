//! Operator enumerations shared across lexer / parser / codegen.

use crate::punctuator::Punctuator;

/// A binary operator (covers arithmetic, comparison, logical and bitwise).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    // Comparison
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Gt,
    Le,
    Ge,
    // Logical
    And,
    Or,
    NullishCoal,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
    // Relational (in / instanceof)
    In,
    Instanceof,
}

impl BinOp {
    /// Map a punctuator to the matching binary operator, if any.
    pub fn from_punctuator(p: Punctuator) -> Option<BinOp> {
        use Punctuator::*;
        Some(match p {
            Add => BinOp::Add,
            Sub => BinOp::Sub,
            Mul => BinOp::Mul,
            Div => BinOp::Div,
            Mod => BinOp::Mod,
            Exp => BinOp::Exp,
            Eq => BinOp::Eq,
            NotEq => BinOp::NotEq,
            StrictEq => BinOp::StrictEq,
            StrictNotEq => BinOp::StrictNotEq,
            Lt => BinOp::Lt,
            Gt => BinOp::Gt,
            Le => BinOp::Le,
            Ge => BinOp::Ge,
            And => BinOp::And,
            Or => BinOp::Or,
            NullishCoal => BinOp::NullishCoal,
            BitAnd => BinOp::BitAnd,
            BitOr => BinOp::BitOr,
            BitXor => BinOp::BitXor,
            Shl => BinOp::Shl,
            Shr => BinOp::Shr,
            Ushr => BinOp::Ushr,
            _ => return None,
        })
    }
}

/// A unary prefix operator.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UnaryOp {
    Neg,    // -x
    Pos,    // +x
    Not,    // !x
    BitNot, // ~x
    Typeof,
    Void,
    Delete,
}

/// An update operator (`++` / `--`).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UpdateOp {
    Inc,
    Dec,
}

/// A compound-assignment operator (`+=`, `&=`, ...).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AssignOp {
    Assign, // =
    Add,    // +=
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
    And,     // &&=
    Or,      // ||=
    Nullish, // ??=
}
