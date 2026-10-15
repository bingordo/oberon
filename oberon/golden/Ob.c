/* Ob.c -- the host/embedding layer (see Ob.h). */
#include "Ob.h"
#include <stddef.h>
#include <string.h>

/* ------------------------------------------------------------------ */
/* the module registry                                                  */
/* ------------------------------------------------------------------ */

#define OB_MAX_MODULES 16

static const char *ob_module_names[OB_MAX_MODULES];
static const ObCommand *ob_module_tables[OB_MAX_MODULES];
static int32_t ob_module_count = 0;

/* The emitted runtime's rt_trap calls this (DESIGN 9.4). The fixture counts
 * traps instead of aborting, which is what makes a trap testable. */
int32_t ob_trap_count = 0;
void ob_trap_report_at(const char *msg, int32_t at) {
  (void)msg;
  (void)at;
  ob_trap_count = ob_trap_count + 1;
}

void ob_register_commands(const char *module, const ObCommand *table) {
  int32_t i;
  for (i = 0; i < ob_module_count; i = i + 1) {
    if (strcmp(ob_module_names[i], module) == 0) {
      ob_module_tables[i] = table; /* re-load replaces the table */
      return;
    }
  }
  if (ob_module_count < OB_MAX_MODULES) {
    ob_module_names[ob_module_count] = module;
    ob_module_tables[ob_module_count] = table;
    ob_module_count = ob_module_count + 1;
  }
}

static const ObCommand *ob_table_of(const char *module) {
  int32_t i;
  for (i = 0; i < ob_module_count; i = i + 1) {
    if (strcmp(ob_module_names[i], module) == 0) return ob_module_tables[i];
  }
  return NULL;
}

const ObCommand *ob_find_command(const char *module, const char *name) {
  const ObCommand *t = ob_table_of(module);
  int32_t i;
  if (t == NULL) return NULL;
  for (i = 0; t[i].name != NULL; i = i + 1) {
    if (strcmp(t[i].name, name) == 0) return &t[i];
  }
  return NULL;
}

int ob_run_command(const char *module, const char *name) {
  const ObCommand *c = ob_find_command(module, name);
  if (c == NULL) return 0;
  c->fn();
  return 1;
}

/* ------------------------------------------------------------------ */
/* Texts, without a GUI                                                 */
/* ------------------------------------------------------------------ */

Texts__Writer Texts__writer;

void Texts__Write(Texts__Writer *w, char ch) {
  if (w->len < (int32_t)sizeof w->buf - 1) {
    w->buf[w->len] = ch;
    w->len = w->len + 1;
    w->buf[w->len] = 0;
  }
}

void Texts__WriteLn(Texts__Writer *w) { Texts__Write(w, '\n'); }

void Texts__Reset(Texts__Writer *w) {
  w->len = 0;
  w->buf[0] = 0;
}
