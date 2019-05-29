#pragma once
#define GLOW_TRUE 1
#define GLOW_FALSE 0

int is_str(const char** code, const char * str);
int parse_statementList(const char** code);

int parse_block(const char** code);

int parse_variableDeclarationList(const char** code);

int parse_variableStatement(const char** code);

int parse_emptyStatement(const char** code);

int parse_expressionStatement(const char** code);
int parse_ifStatement(const char** code);
int parse_iterationStatement(const char** code);
int parse_continueStatement(const char** code);
int parse_breakStatement(const char** code);
int parse_returnStatement(const char** code);
int parse_withStatement(const char** code);
int parse_labelledStatement(const char** code);
int parse_switchStatement(const char** code);
int parse_throwStatement(const char** code);
int parse_tryStatement(const char** code);

int parse_statement(const char** code);

int parse_functionDeclaration(const char** code);
int parse_program(const char** code);
int parse_sourceElement(const char** code);

int parse_sourceElements(const char** code);