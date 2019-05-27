
#include "glowjs.h"

struct glow_State * glow_newState()
{
	struct glow_State * ret = GLOW_NULL;
	ret = (struct glow_State *)malloc(1 * sizeof(struct glow_State));
	return ret;
}

void glow_deleteState(struct glow_State * state)
{

}

void glow_doString(const char *code)
{

}

