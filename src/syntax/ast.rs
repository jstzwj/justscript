use std::boxed::Box;

#[derive(Debug)]
pub struct variableDeclaration {
    pub identifier: String,
    // pub initialiser: AssignmentExpression,
}

#[derive(Debug)]
pub struct VariableDeclarationList {
    list: Vec<variableDeclaration>
}

#[derive(Debug)]
pub struct AssignmentExpression {

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
    VariableStatement(VariableDeclarationList),
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