//! Expression AST nodes.

use crate::ast::lit::Lit;
use crate::ast::op::{AssignOp, BinOp, UnaryOp, UpdateOp};
use crate::ast::pat::PropKey;
use crate::Span;

/// A boxed expression, used pervasively to break recursive type sizes.
pub type BoxExpr = Box<Expr>;

/// An ECMAScript expression.
#[derive(Clone, Debug)]
pub enum Expr {
    /// `this`
    This(Span),
    /// A super reference (`super`, `super.x`, `super(...)`).
    Super(Span),
    /// An identifier reference, e.g. `foo`.
    Ident { span: Span, name: String },
    /// A literal.
    Lit(Lit),
    /// A template literal with interpolation.
    TemplateLit {
        span: Span,
        quasis: Vec<(Option<String>, String)>,
        expressions: Vec<Expr>,
    },
    /// An array literal `[a, b, ...c]`.
    Array {
        span: Span,
        elements: Vec<Option<ArrayExprElement>>,
    },
    /// An object literal `{ a, b: c, ...d }`.
    Object { span: Span, props: Vec<ObjectProp> },
    /// A function expression.
    Function(Box<FunctionExpr>),
    /// An arrow function.
    Arrow(Box<ArrowExpr>),
    /// A class expression.
    Class(Box<ClassExpr>),
    /// A regular expression literal.
    Regex {
        span: Span,
        pattern: String,
        flags: String,
    },
    /// A parenthesized expression (transparent; kept for spans).
    Paren { span: Span, expr: BoxExpr },
    /// Unary prefix `-x`, `!x`, `typeof x`, ...
    Unary { span: Span, op: UnaryOp, arg: BoxExpr },
    /// `++x` / `--x` (prefix) and `x++` / `x--` (postfix).
    Update {
        span: Span,
        op: UpdateOp,
        prefix: bool,
        arg: BoxExpr,
    },
    /// A binary operation `a + b`.
    Binary { span: Span, op: BinOp, left: BoxExpr, right: BoxExpr },
    /// A logical short-circuit chain, used for `a && b && c`.
    Logical { span: Span, op: BinOp, left: BoxExpr, right: BoxExpr },
    /// A conditional / ternary `c ? a : b`.
    Conditional {
        span: Span,
        test: BoxExpr,
        cons: BoxExpr,
        alt: BoxExpr,
    },
    /// An assignment `lhs op rhs`.
    Assign {
        span: Span,
        op: AssignOp,
        left: AssignTarget,
        right: BoxExpr,
    },
    /// A sequence `a, b, c`.
    Sequence { span: Span, exprs: Vec<Expr> },
    /// A member access `obj.prop` or `obj[expr]`.
    Member(Box<MemberExpr>),
    /// A call `callee(args)`.
    Call(Box<CallExpr>),
    /// `new C(args)`.
    New(Box<NewExpr>),
    /// Tagged template `tag\`...\``.
    TaggedTemplate {
        span: Span,
        tag: BoxExpr,
        template: Box<Expr>,
    },
    /// Spread / rest in an argument or array position: `...x`.
    Spread { span: Span, arg: BoxExpr },
    /// `yield expr` / `yield* expr`.
    Yield { span: Span, arg: Option<BoxExpr>, delegate: bool },
    /// `await expr`.
    Await { span: Span, arg: BoxExpr },
}

impl Expr {
    /// The byte span this expression covers in the source.
    pub fn span(&self) -> Span {
        match self {
            Expr::This(s) | Expr::Super(s) => *s,
            Expr::Lit(l) => l.span(),
            Expr::Ident { span, .. }
            | Expr::TemplateLit { span, .. }
            | Expr::Array { span, .. }
            | Expr::Object { span, .. }
            | Expr::Regex { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Update { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Logical { span, .. }
            | Expr::Conditional { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Sequence { span, .. }
            | Expr::TaggedTemplate { span, .. }
            | Expr::Spread { span, .. }
            | Expr::Yield { span, .. }
            | Expr::Await { span, .. } => *span,
            Expr::Function(f) => f.span,
            Expr::Arrow(a) => a.span,
            Expr::Class(c) => c.span,
            Expr::Member(m) => m.span,
            Expr::Call(c) => c.span,
            Expr::New(n) => n.span,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ArrayExprElement {
    Expr(Expr),
    Spread(Expr),
}

/// The left-hand side of an assignment (after destructuring analysis).
#[derive(Clone, Debug)]
pub enum AssignTarget {
    Ident { span: Span, name: String },
    Member(Box<MemberExpr>),
    /// A destructuring pattern — resolved to a [`crate::ast::pat::Pat`] by the parser.
    Pat(crate::ast::pat::Pat),
}

#[derive(Clone, Debug)]
pub struct MemberExpr {
    pub span: Span,
    pub object: BoxExpr,
    pub property: MemberProp,
}

#[derive(Clone, Debug)]
pub enum MemberProp {
    /// `obj.name`
    Ident(String),
    /// `obj.#name`
    Private(String),
    /// `obj[expr]`
    Computed(BoxExpr),
}

#[derive(Clone, Debug)]
pub struct CallExpr {
    pub span: Span,
    pub callee: BoxExpr,
    pub args: Vec<CallArg>,
    /// `true` for optional calls `callee?.(...)`.
    pub optional: bool,
}

#[derive(Clone, Debug)]
pub enum CallArg {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Clone, Debug)]
pub struct NewExpr {
    pub span: Span,
    pub callee: BoxExpr,
    pub args: Vec<CallArg>,
}

#[derive(Clone, Debug)]
pub struct ObjectProp {
    pub span: Span,
    pub key: PropKey,
    pub value: ObjectPropValue,
    pub computed: bool,
    pub method: bool,
    pub shorthand: bool,
    /// `get` / `set` accessor, if any.
    pub kind: ObjectPropKind,
}

#[derive(Clone, Debug)]
pub enum ObjectPropValue {
    Expr(Expr),
    /// `get prop() {}` / `set prop(v) {}`.
    Method(Box<FunctionExpr>),
    Spread(Expr),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ObjectPropKind {
    #[default]
    Init,
    Get,
    Set,
}

/// A function (declaration or expression) body.
#[derive(Clone, Debug)]
pub struct Function {
    pub span: Span,
    pub name: Option<String>,
    pub params: Vec<crate::ast::pat::Pat>,
    pub body: Vec<crate::ast::stmt::Stmt>,
    pub is_async: bool,
    pub is_generator: bool,
}

pub type FunctionExpr = Function;
pub type FunctionDecl = Function;

/// An arrow function. The body is either a block or a single expression
/// (implicit-return shorthand).
#[derive(Clone, Debug)]
pub struct ArrowExpr {
    pub span: Span,
    pub params: Vec<crate::ast::pat::Pat>,
    pub body: ArrowBody,
    pub is_async: bool,
}

#[derive(Clone, Debug)]
pub enum ArrowBody {
    Block(Vec<crate::ast::stmt::Stmt>),
    Expr(BoxExpr),
}

/// A class expression / declaration.
#[derive(Clone, Debug)]
pub struct Class {
    pub span: Span,
    pub name: Option<String>,
    pub superclass: Option<BoxExpr>,
    pub body: Vec<ClassMember>,
}

pub type ClassExpr = Class;
pub type ClassDecl = Class;

#[derive(Clone, Debug)]
pub struct ClassMember {
    pub span: Span,
    pub key: PropKey,
    pub value: ClassMemberValue,
    pub static_: bool,
    pub computed: bool,
    pub kind: ClassMemberKind,
}

#[derive(Clone, Debug)]
pub enum ClassMemberValue {
    Method(Box<FunctionExpr>),
    Field(Option<Expr>),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ClassMemberKind {
    #[default]
    Method,
    Constructor,
    Get,
    Set,
    Field,
}
