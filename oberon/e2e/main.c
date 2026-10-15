#include <stdio.h>
#include "Texts.h"
#include "Oberon.h"
#include "Out.h"
#include "Torture.h"

int main(void) {
    /* A host loads modules in dependency order and then activates commands. */
    Texts__load();
    Oberon__load();
    Out__load();
    Torture__load();
    if (!ob_run_command("Torture", "Run")) {
        fprintf(stderr, "command Run not found\n");
        return 2;
    }
    return 0;
}
