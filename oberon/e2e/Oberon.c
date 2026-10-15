#include "Oberon.h"

Texts__Writer Oberon__Log;

void Oberon__init(void) {
    /* The report's Oberon module opens its log writer when it loads. */
    Texts__OpenWriter(&Oberon__Log);
}

void Oberon__load(void) {
    Oberon__init();
}
