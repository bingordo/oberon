/* The ABI across back ends (DESIGN §16): MixLib and MixC2 are the C back end's
   objects, MixRoot and MixLeaf the second back end's, and they are linked into
   one program. All the checking is in the modules' own ASSERTs, so the exit
   status is the verdict. MixLib.h and MixC2.h are the C back end's to write;
   the second back end writes no header, so its entries are declared here. */
#include "MixLib.h"
#include "MixC2.h"
extern void MixRoot__load(void);
extern void MixLeaf__load(void);

int main(void) {
    MixLib__load();   /* C */
    MixLeaf__load();  /* clif, called by MixC2 */
    MixRoot__load();  /* clif, calls MixLib: an argument in, a result out */
    MixC2__load();    /* C, calls MixLeaf: a result out of a clif object */
    return 0;
}
