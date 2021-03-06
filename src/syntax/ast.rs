use std::boxed::Box;

#[derive(Debug)]
pub struct VariableDeclaration {
    pub identifier: String,
    pub initializer: AssignmentExpression,
}

impl VariableDeclaration {
    pub fn new() -> VariableDeclaration {
        VariableDeclaration {
            identifier: String::new(),
            initializer: AssignmentExpression::ConditionalExpression
        }
    }
}

#[derive(Debug)]
pub struct VariableStatement {
    pub variable_declaration_list: Vec<VariableDeclaration>
}

impl VariableStatement {
    pub fn new() -> VariableStatement {
        VariableStatement {
            variable_declaration_list: Vec::new()
        }
    }
}

#[derive(Debug)]
pub enum BreakableStatement {
    IterationStatement(IterationStatement),
    SwitchStatement(SwitchStatement),
}

#[derive(Debug)]
pub struct IterationStatement {
}

#[derive(Debug)]
pub struct SwitchStatement {
}

#[derive(Debug)]
pub enum AssignmentExpression {
    ConditionalExpression,
    YieldExpression,
    ArrowFunction,
    AsyncArrowFunction,
    LeftHandSideAssignmentExpression(LeftHandSideAssignmentExpression),
}

#[derive(Debug)]
pub struct LeftHandSideAssignmentExpression {
    pub left_hand_side: Box<LeftHandSideExpression>,
    pub assignment_op: AssignmentOperator,
    pub right_hand_side: Box<AssignmentExpression>,
}

#[derive(Debug)]
pub enum LeftHandSideExpression {
    NewExpression(NewExpression),
    CallExpression(CallExpression),
}

#[derive(Debug)]
pub enum NewExpression {
    MemberExpression(Box<MemberExpression>),
    NewExpression(Box<NewExpression>),
}

#[derive(Debug)]
pub enum CallExpression {
    
}

#[derive(Debug)]
pub enum MemberExpression {
    PrimaryExpression(Box<PrimaryExpression>),
}

#[derive(Debug)]
pub enum AssignmentOperator {
    /// =
    Equal,
    /// *=
    TimesEq,
    /// /=
    DivideEq,
    /// %=
    PercentEq,
}

#[derive(Debug)]
pub enum PrimaryExpression {
    This,
    IdentifierReference(IdentifierReference),
    Literal(Literal),
    ArrayLiteral,
    ObjectLiteral,
    FunctionExpression,
    ClassExpression,
    GeneratorExpression,
    AsyncFunctionExpression,
    AsyncGeneratorExpression,
    RegularExpressionLiteral,
    TemplateLiteral,
    CoverParenthesizedExpressionAndArrowParameterList,
}

#[derive(Debug)]
pub enum IdentifierReference {
    Identifier(String),
    Yield,
    Await,
}

#[derive(Debug)]
pub enum Literal {
    NullLiteral,
    BooleanLiteral(bool),
    NumericLiteral(NumericLiteral),
    StringLiteral(String),
}

#[derive(Debug)]
pub enum NumericLiteral {
    DecimalLiteral(f64),
    BinaryIntegerLiteral(i32),
    OctalIntegerLiteral(i32),
    HexIntegerLiteral(i32),
}

#[derive(Debug)]
pub struct ConditionalExpression {

}

#[derive(Debug)]
pub struct Expression {

}

#[derive(Debug)]
pub enum Statement {
    BlockStatement,
    VariableStatement(VariableStatement),
    EmptyStatement,
    ExpressionStatement,
    ContinueStatement,
    BreakableStatement(BreakableStatement),
    ReturnStatement,
    ThrowStatement,
    DebuggerStatement,
}

#[derive(Debug)]
pub struct Declaration {

}

#[derive(Debug)]
pub enum StatementListItem {
    Statement(Statement),
    Declaration(Declaration),
}

#[derive(Debug)]
pub struct StatementList {
    pub items: Vec<StatementListItem>
}

impl StatementList {
    pub fn new() -> StatementList {
        StatementList {
            items: Vec::new()
        }
    }
}