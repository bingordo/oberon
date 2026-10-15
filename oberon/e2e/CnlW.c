/* Generated from CnlW.Mod. */
#include "CnlW.h"
static int32_t CnlW__i;
static const ObCommand CnlW__commands[] = {
    { NULL, NULL }
};
void CnlW__init(void) {
    (void)CnlW__i;
    CnlW__i = ((int32_t)1u);
    switch (CnlW__i) {
    case 1: {
    CnlW__i = ((int32_t)2u);
    break; }
    default: rt_trap_pos("CASE: no matching label", 54); break;
    }
}
void CnlW__load(void) {
    CnlW__init();
    ob_register_commands("CnlW", CnlW__commands);
}
