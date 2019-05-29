#include <stdio.h>
#include "glowjs.h"
#include "glowparser.h"

int main(int argc, char **argv)
{
	/*
	char line[256];
	glow_State *J = glow_newState();
	while (fgets(line, sizeof line, stdin))
		js_dostring(J, line);
	glow_deleteState(J);
	*/

	const char code[] = "var abc = 1";
	const char* code_ptr = code;
	parse_program(&code_ptr);
}