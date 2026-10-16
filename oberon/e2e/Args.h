/* The Args module, whose body is in the runtime (DESIGN 11). */
#ifndef ARGS_H
#define ARGS_H
#include "oberon.h"
int32_t Args__Count(void);
void Args__Get(int32_t i, char *s, int32_t s__len);
#endif
