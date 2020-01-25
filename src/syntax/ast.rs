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
    pub variableDeclarationList: Vec<VariableDeclaration>
}

impl VariableStatement {
    pub fn new() -> VariableStatement {
        VariableStatement {
            variableDeclarationList: Vec::new()
        }
    }
}

#[derive(Debug)]
pub enum AssignmentExpression {
    ConditionalExpression,
    YieldExpression,
    ArrowFunction,
    AsyncArrowFunction,
    LeftHandSideAssignmentExpression(LeftHandSideAssignmentExpression),
    LeftHandSideAssignmentOperator(LeftHandSideAssignmentOperator),
}

#[derive(Debug)]
pub struct LeftHandSideAssignmentExpression {

}

#[derive(Debug)]
pub struct LeftHandSideAssignmentOperator {
    
}

#[derive(Debug)]
pub enum LeftHandSideExpression {

}

#[derive(Debug)]
pub enum NewExpression {

}

#[derive(Debug)]
pub enum CallExpression {

}

#[derive(Debug)]
pub enum MemberExpression {

}

#[derive(Debug)]
pub enum AssignmentOperator {
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
    BreakStatement,
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