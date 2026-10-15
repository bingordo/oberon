/* harness.h -- the tiny PASS/FAIL harness, the same shape as oberon/csmoke:
 * one "PASS <case>" or "FAIL <case>: <detail>" line per check, a summary line,
 * and a non-zero exit status from main when anything failed. */
#ifndef HARNESS_H
#define HARNESS_H

#include <stdint.h>

void check(const char *name, int32_t cond, const char *detail);
/* check a captured string against its expected text */
void check_str(const char *name, const char *got, const char *expected);
int32_t harness_failures(void);
void harness_summary(const char *label);

#endif /* HARNESS_H */
