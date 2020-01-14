pub enum Statement {
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

pub enum SourceElement {
    Statement(Statement),
    FunctionDeclaration(),
}

pub struct Source {
    pub elements: Vec<SourceElement>
}

impl Source {
    pub fn new() -> Source {
        Source {
            elements: Vec::new()
        }
    }
}