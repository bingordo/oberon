#include "Texts.h"
#include "Hello.h"

int main(void) {
    /* The module body itself says hello when the module is loaded. */
    Texts__load();
    Hello__load();
    /* And a parameterless exported procedure is a command (report 11). */
    ob_run_command("Hello", "Greet");
    return 0;
}
