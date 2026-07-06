//! The opcode set.
//!
//! Stack-based with explicit local slots, modeled loosely on V8/Lua: operands
//! pull from / push to the operand stack, and named locals are accessed by a
//! 16-bit slot index. Each opcode lowers to a short Cranelift sequence.

use js_syntax::ast::op::{BinOp, UnaryOp};

/// A single instruction: an [`Opcode`] plus a 16-bit immediate operand.
///
/// Immediates are interpreted per-opcode: slot index, constant-pool index,
/// jump target (instruction index), or argument count.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Instruction {
    pub op: Opcode,
    pub operand: u16,
}

impl Instruction {
    pub const fn new(op: Opcode, operand: u16) -> Instruction {
        Instruction { op, operand }
    }
    pub const fn bare(op: Opcode) -> Instruction {
        Instruction { op, operand: 0 }
    }
}

/// The opcode set.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Opcode {
    // --- stack / locals -------------------------------------------------
    /// Push constant pool entry `operand` onto the stack.
    LdaConst,
    /// Load local slot `operand` and push it.
    LdaLocal,
    /// Pop and store into local slot `operand`.
    StaLocal,
    /// Pop the top of the stack (discard).
    Pop,
    /// Duplicate the top of the operand stack.
    Dup,
    /// `undefined`
    LdaUndefined,
    /// `null`
    LdaNull,
    LdaTrue,
    LdaFalse,
    /// Push a function value (function-table index `operand`).
    LdaFunction,

    // --- arithmetic / binary (operands popped, result pushed) -----------
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    // comparison
    Eq,
    StrictEq,
    Lt,
    Le,
    Gt,
    Ge,
    // bitwise
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    // logical short-circuit (handled specially in the interp/codegen)
    LogicalAnd,
    LogicalOr,
    NullishCoal,

    // --- unary ----------------------------------------------------------
    Neg,
    Pos,
    Not,
    BitNot,
    Typeof,

    // --- control flow ---------------------------------------------------
    /// Unconditional jump to instruction index `operand`.
    Jump,
    /// Pop; jump if falsy.
    JumpIfFalse,
    /// Pop; jump if truthy.
    JumpIfTrue,
    /// `return` (pops the return value if any; pushes undefined otherwise).
    Return,

    // --- objects / calls ------------------------------------------------
    /// Create a fresh object and push it.
    NewObject,
    /// Create an array with `operand` elements popped from the stack.
    NewArray,
    /// `callee`, pop `operand` args, push result.
    Call,
    /// `new` call with `operand` args.
    New,
    /// Pop object + key, push the property.
    GetProp,
    /// Pop value + key + object.
    SetProp,
    /// Push the named global `operand` (constant index).
    GetGlobal,
    /// Set the named global `operand`.
    SetGlobal,

    // --- bookkeeping ----------------------------------------------------
    /// No-op / padding.
    Nop,
}

impl Opcode {
    /// Map a parsed [`BinOp`] to its lowering opcode, where possible.
    pub fn for_binop(op: BinOp) -> Opcode {
        use BinOp::*;
        match op {
            Add => Opcode::Add,
            Sub => Opcode::Sub,
            Mul => Opcode::Mul,
            Div => Opcode::Div,
            Mod => Opcode::Mod,
            Exp => Opcode::Exp,
            Eq => Opcode::Eq,
            NotEq => Opcode::Eq, // negated by caller
            StrictEq => Opcode::StrictEq,
            StrictNotEq => Opcode::StrictEq, // negated by caller
            Lt => Opcode::Lt,
            Gt => Opcode::Gt,
            Le => Opcode::Le,
            Ge => Opcode::Ge,
            And => Opcode::LogicalAnd,
            Or => Opcode::LogicalOr,
            NullishCoal => Opcode::NullishCoal,
            BitAnd => Opcode::BitAnd,
            BitOr => Opcode::BitOr,
            BitXor => Opcode::BitXor,
            Shl => Opcode::Shl,
            Shr => Opcode::Shr,
            Ushr => Opcode::Shr, // TODO: unsigned variant
            In | Instanceof => Opcode::Nop, // runtime-call lowering, TODO
        }
    }

    /// Map a parsed [`UnaryOp`] to its lowering opcode.
    pub fn for_unaryop(op: UnaryOp) -> Opcode {
        use UnaryOp::*;
        match op {
            Neg => Opcode::Neg,
            Pos => Opcode::Pos,
            Not => Opcode::Not,
            BitNot => Opcode::BitNot,
            Typeof => Opcode::Typeof,
            Void | Delete => Opcode::Nop, // TODO
        }
    }
}
