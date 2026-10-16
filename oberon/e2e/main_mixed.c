/* The ABI across back ends (DESIGN section 16): MixLib and MixC2 are the C
   back end's objects. MixRoot and MixLeaf belong to the second back end, and
   all four are linked into one program. ASSERTs in the modules do the
   checking, so the exit status is the verdict. MixLib.h and MixC2.h are the
   C back end's to write; the second back end doesn't write a header, so its
   entries are declared here. */
#include "MixLib.h"
#include "MixC2.h"
extern void MixRoot__load(void);
extern void MixLeaf__load(void);

int main(void) {
    MixLib__load();   /* C */
    MixLeaf__load();  /* clif, called by MixC2 */
    MixRoot__load();  /* clif calls MixLib with an argument */
    MixC2__load();    /* C calls MixLeaf and gets a result */
    return 0;
}
