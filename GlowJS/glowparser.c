#include "glowparser.h"

#define LBRACK "["
#define RBRACK "]"
#define LBRACE "{"
#define RBRACE "}"
#define LPAREN "("
#define RPAREN ")"

int is_str(const char** code, const char * str)
{
	const char* start = *code;
	while (1)
	{
		if (*str == '\0')
			return GLOW_TRUE;
		if (**code == '\0')
		{
			*code = start;
			return GLOW_FALSE;
		}
		(*code)++;
		str++;
	}
}

int parse_statementList(const char** code)
{

}

int parse_block(const char** code)
{
	if (!is_str(code, LBRACE))
		return GLOW_FALSE;

	if (!is_str(code, RBRACE))
		return GLOW_FALSE;
}

int parse_variableStatement(const char** code)
{

}

int parse_emptyStatement(const char** code)
{

}

int parse_expressionStatement(const char** code){}
int parse_ifStatement(const char** code){}
int parse_iterationStatement(const char** code){}
int parse_continueStatement(const char** code){}
int parse_breakStatement(const char** code){}
int parse_returnStatement(const char** code){}
int parse_withStatement(const char** code){}
int parse_labelledStatement(const char** code){}
int parse_switchStatement(const char** code){}
int parse_throwStatement(const char** code){}
int parse_tryStatement(const char** code){}

int parse_statement(const char** code)
{
	if (parse_block(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_variableStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_emptyStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_expressionStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_ifStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_iterationStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_continueStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_breakStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_returnStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_withStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_labelledStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_switchStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_throwStatement(code))
	{
		return GLOW_TRUE;
	}
	else if (parse_tryStatement(code))
	{
		return GLOW_TRUE;
	}
	else
	{
		return GLOW_FALSE;
	}
}

int parse_functionDeclaration(const char** code)
{

}

int parse_sourceElement(const char** code)
{
	int statement_ret = GLOW_FALSE;
	int functionDeclaration_ret = GLOW_FALSE;

	statement_ret = parse_statement(code);
	if (!statement_ret)
	{
		functionDeclaration_ret = parse_statement(code);
		if (!functionDeclaration_ret)
		{
			return GLOW_FALSE;
		}
	}

	return GLOW_TRUE;
}

int parse_sourceElements(const char** code)
{
	while (**code)
	{
		parse_sourceElement(code);
	}

	return GLOW_TRUE;
}