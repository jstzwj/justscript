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

pub struct variableDeclaration {
    pub identifier: String,
    // pub initialiser: AssignmentExpression,
}

pub struct VariableDeclarationList {
    list: Vec<variableDeclaration>
}

pub struct AssignmentExpression {

}

pub struct ConditionalExpression {

}

pub struct Expression {

}

pub enum StatementListItem {
    Statement(Statement),
    FunctionDeclaration(),
}

pub struct StatementList {
    pub elements: Vec<StatementListItem>
}

impl StatementList {
    pub fn new() -> StatementList {
        StatementList {
            elements: Vec::new()
        }
    }
}