/* harness.c -- see harness.h */
#include "harness.h"
#include <stdio.h>
#include <string.h>

static int32_t total = 0;
static int32_t failed = 0;

void check(const char *name, int32_t cond, const char *detail) {
  total = total + 1;
  if (cond) {
    printf("PASS %s\n", name);
  } else {
    printf("FAIL %s: %s\n", name, detail);
    failed = failed + 1;
  }
}

void check_str(const char *name, const char *got, const char *expected) {
  char detail[256];
  detail[0] = 0;
  if (strcmp(got, expected) != 0) {
    snprintf(detail, sizeof detail, "expected \"%s\", got \"%s\"", expected, got);
  }
  check(name, detail[0] == 0, detail);
}

int32_t harness_failures(void) { return failed; }

void harness_summary(const char *label) {
  printf("SUMMARY %s: %d case(s), %d failed\n", label, (int)total, (int)failed);
}
