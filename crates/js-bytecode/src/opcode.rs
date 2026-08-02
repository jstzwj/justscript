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
    /// Swap the top two operands.
    Swap,
    /// `undefined`
    LdaUndefined,
    /// `null`
    LdaNull,
    LdaTrue,
    LdaFalse,
    /// Push a function value (function-table index `operand`).
    LdaFunction,
    /// Push the current `this` binding.
    LdaThis,
    /// Load captured upvalue slot `operand` and push it.
    LdaUpvalue,
    /// Pop and store into captured upvalue slot `operand`.
    StaUpvalue,

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
    Ushr,
    /// ECMAScript `in`: tests a property key against an object and its
    /// prototype chain. Throws when the right operand is not an object.
    In,
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
    /// Evaluate the operand for side effects and produce `undefined`.
    Void,

    // --- control flow ---------------------------------------------------
    /// Unconditional jump to instruction index `operand`.
    Jump,
    /// Pop; jump if falsy.
    JumpIfFalse,
    /// Pop; jump if truthy.
    JumpIfTrue,
    /// `return` (pops the return value if any; pushes undefined otherwise).
    Return,
    /// `yield` (generator): pop the yielded value, suspend the frame, push the
    /// value passed to the next `.next(v)` onto resumption.
    Yield,
    /// `await`: perform Promise resolution and a job-queue checkpoint, then
    /// push the fulfilled value or throw the rejection reason.
    Await,
    /// `throw expr` (pops a value): raise it as an exception.
    Throw,
    /// Push an exception handler (catch_pc + finally_pc from the function's
    /// handler table at index `operand`).
    TryBegin,
    /// Pop the innermost exception handler (normal exit of a protected region).
    TryEnd,
    /// End of a `finally` block: re-raise `pending_throw` if set, else continue.
    FinallyEnd,
    /// Pop an iterable; push an iterator over it (generator→itself;
    /// array/string→an array-iterator object).
    GetIterator,
    /// Pop an iterator; step it once, leaving an iterator-result `{value, done}`
    /// on the stack. For generators this drives a `.next()` (the result lands
    /// when the generator yields/returns).
    IterNext,

    // --- objects / calls ------------------------------------------------
    /// Create a fresh object and push it.
    NewObject,
    /// Create an array with `operand` elements popped from the stack.
    NewArray,
    /// `callee`, pop `operand` args, push result.
    Call,
    /// Method call: stack `[obj, fn, args...]`, `this` = obj.
    CallMethod,
    /// `new` call with `operand` args.
    New,
    /// Pop object + key, push the property.
    GetProp,
    /// Pop value + key + object.
    SetProp,
    /// Delete an own property. Stack input is `[object, key]`; pushes a bool.
    DeleteProp,
    /// Delete an unqualified global reference named by a string constant.
    DeleteGlobal,
    /// `a instanceof B`: pop B, pop a, push boolean.
    Instanceof,
    /// Create a RegExp object from constant `operand` (pattern + "\0" + flags).
    NewRegex,
    /// Push the named global `operand` (constant index).
    GetGlobal,
    /// Set the named global `operand`.
    SetGlobal,

    // --- bookkeeping ----------------------------------------------------
    /// No-op / padding.
    Nop,
    /// Pop an object/array; push a fresh array of its enumerable own key
    /// strings (numeric indices for arrays). Used by `for-in`.
    ObjectKeys,
    /// Stack `[arr, value]` → `[arr]`: append `value` to the array.
    ArrayPush,
    /// Stack `[arr, src]` → `[arr]`: append every element of `src` to `arr`.
    ArrayExtend,
}

/// The meaning of [`Instruction::operand`] for an opcode.
///
/// Keeping this metadata beside the opcode list gives the verifier, debugger,
/// interpreter and future native backends one authoritative bytecode contract.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum OperandKind {
    None,
    Constant,
    Local,
    Upvalue,
    Function,
    JumpTarget,
    ArgumentCount,
    Handler,
}

impl Opcode {
    pub const fn operand_kind(self) -> OperandKind {
        use OperandKind::*;
        match self {
            Opcode::LdaConst
            | Opcode::NewRegex
            | Opcode::GetGlobal
            | Opcode::SetGlobal
            | Opcode::DeleteGlobal => Constant,
            Opcode::LdaLocal | Opcode::StaLocal => Local,
            Opcode::LdaUpvalue | Opcode::StaUpvalue => Upvalue,
            Opcode::LdaFunction => Function,
            Opcode::Jump | Opcode::JumpIfFalse | Opcode::JumpIfTrue => JumpTarget,
            Opcode::Call | Opcode::CallMethod | Opcode::New | Opcode::NewArray => ArgumentCount,
            Opcode::TryBegin => Handler,
            _ => None,
        }
    }

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
            Ushr => Opcode::Ushr,
            In => Opcode::In,
            Instanceof => Opcode::Instanceof,
        }
    }

    /// Map a parsed [`UnaryOp`] to its lowering opcode.
    pub fn for_unaryop(op: UnaryOp) -> Option<Opcode> {
        use UnaryOp::*;
        Some(match op {
            Neg => Opcode::Neg,
            Pos => Opcode::Pos,
            Not => Opcode::Not,
            BitNot => Opcode::BitNot,
            Typeof => Opcode::Typeof,
            Void => Opcode::Void,
            Delete => return None,
        })
    }
}
