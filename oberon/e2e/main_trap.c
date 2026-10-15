#include "Texts.h"
#include "Oberon.h"
#include "Trap.h"
int main(void) {
    Oberon__load();
    Trap__load();
    ob_run_command("Trap", "Run");
    return 0;
}
