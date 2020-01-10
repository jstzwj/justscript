pub enum Statement {
    VariableStatement,
    EmptyStatement,
    ExpressionStatement,
    ContinueStatement,
    BreakStatement,
    ReturnStatement,
    ThrowStatement,
    DebuggerStatement,
}

pub enum SourceElement {
    Statement(Statement),
    FunctionDeclaration(),
}
