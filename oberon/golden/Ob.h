/* Ob.h -- the host/embedding layer of DESIGN.md 9.7 ("Commands") and the
 * stub for the Texts module the fixture's Out needs to run without a GUI.
 *
 * This is *not* generated code: it is the fixed runtime the generated modules
 * link against, so the generated shapes in Out.c can be exercised.
 */
#ifndef OB_H
#define OB_H

#include <stdint.h>

/* The types and the two host entry points come from the runtime the compiler
 * actually emits, so this fixture cannot drift from it (DESIGN §14). */
#include "../src/oberon_h.inc"

/* Find one entry without calling it.
 * Not part of the design's host API (which is exactly the two functions above).
 * The fixture needs it to show the one honest way to reach a command that has
 * parameters: ob_run_command can only call `void (*)(void)`, so a host that
 * knows the real Oberon signature finds the entry and re-casts the stored
 * pointer back to it (C11 6.3.2.3p8: a function pointer may be converted to
 * another function pointer type and back, and the round trip compares equal). */
const ObCommand *ob_find_command(const char *module, const char *name);

/* --- stand-in for Texts (the report's writer), minus any GUI ---------------
 * DESIGN 9.8's Out writes through a Texts.Writer; a writer is a growable
 * character buffer.  The fixture keeps exactly one. */
typedef struct { char buf[4096]; int32_t len; } Texts__Writer;

extern Texts__Writer Texts__writer; /* the one host writer */

void Texts__Write(Texts__Writer *w, char ch); /* Texts.Write(W, ch) */
void Texts__WriteLn(Texts__Writer *w);        /* Texts.WriteLn(W) */
void Texts__Reset(Texts__Writer *w);          /* fixture convenience: empty it */

#endif /* OB_H */
