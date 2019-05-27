#pragma once

#include <stdlib.h>

#define GLOW_NULL 0

struct glow_State
{
	int state_id;
};


struct glow_State * glow_newState();

void glow_deleteState(struct glow_State * state);

void glow_doString(const char *code);