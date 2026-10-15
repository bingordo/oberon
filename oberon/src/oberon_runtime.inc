/* oberon_runtime.c -- the part of the runtime that must be shared by every
 * generated module: the trap reporter and the command registry (report 11).
 *
 * Link this once with the generated translation units.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <stdint.h>

#include "oberon.h"

/* One report line per trap, then abort: a failed guard, a bounds violation, a
 * NIL dereference or a CASE with no match (DESIGN 9.4). */
void ob_trap_report_at(const char *msg, int32_t at) {
    fflush(stdout);
    if (at < 0) {
        fprintf(stderr, "oberon: trap: %s\n", msg);
    } else {
        fprintf(stderr, "oberon: trap: %s (at byte %d of the source)\n", msg, (int)at);
    }
}

/* Commands, as report 11 defines them: parameterless procedures of a module,
 * activatable by name once the module is loaded. A small fixed table is enough
 * for a command-oriented system and keeps the runtime allocation-free. */
#define OB_MAX_MODULES 32
#define OB_MAX_COMMANDS 64

struct ObRegistry {
    const char *module;
    const ObCommand *table;
};

static struct ObRegistry ob_registry[OB_MAX_MODULES];
static int ob_registry_len = 0;

void ob_register_commands(const char *module, const ObCommand *table) {
    int i;
    for (i = 0; i < ob_registry_len; i++) {
        if (strcmp(ob_registry[i].module, module) == 0) {
            ob_registry[i].table = table;
            return;
        }
    }
    if (ob_registry_len == OB_MAX_MODULES) {
        ob_trap_report_at("too many modules registered", -1);
        abort();
    }
    ob_registry[ob_registry_len].module = module;
    ob_registry[ob_registry_len].table = table;
    ob_registry_len = ob_registry_len + 1;
}

int ob_run_command(const char *module, const char *name) {
    int i;
    for (i = 0; i < ob_registry_len; i++) {
        if (strcmp(ob_registry[i].module, module) == 0) {
            const ObCommand *c = ob_registry[i].table;
            for (; c != NULL && c->name != NULL; c++) {
                if (strcmp(c->name, name) == 0) {
                    c->fn();
                    return 1;
                }
            }
            return 0;
        }
    }
    return 0;
}
