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
            initializer: AssignmentExpression::new()
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
    LeftHandSideAssignmentExpression,
    LeftHandSideAssignmentOperator,
}
/*
impl AssignmentExpression {
    pub fn new() -> AssignmentExpression {
        AssignmentExpression {
        }
    }
}
*/

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