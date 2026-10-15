/* Out.h -- the emitted interface of module Out (DESIGN 9.8): the module body,
 * the loader, the exported procedure prototypes, and the exported variables.
 * Hand-written in the shape the emitter must produce. */
#ifndef OUT_H
#define OUT_H

#include <stdint.h>
#include "Ob.h" /* Texts__Writer, used by the exported variable below */

/* --- exported procedures (file-scope, external linkage) --- */
void Out__init(void);                          /* the module body */
void Out__load(void);                          /* Out__init + command table registration */
void Out__Write(char ch);                      /* PROCEDURE Write(ch: CHAR) */
void Out__WriteInt(int32_t x, int32_t n);      /* PROCEDURE WriteInt(x, n: INTEGER) */
void Out__WriteLn(void);                       /* PROCEDURE WriteLn */

/* --- exported variables ---
 * SHAPE CHOSEN: a plain `extern` declaration here, with the single definition
 * in Out.c, and no accessor.  DESIGN 9.8/§7 say an exported variable is
 * read-only for importers, but C cannot enforce that: declaring it `const`
 * here would collide with Out.c's own assignment (`Out__done := TRUE` in the
 * module body), and declaring it `const` in Out.c instead would make the
 * importer's read of a `const`-qualified object the only legal use while
 * still not stopping `*(int32_t *)&Out__done = 0`.  The rule therefore stays a
 * front-end (resolver) rule; see the note in Out.c. */
extern Texts__Writer *Out__W; /* VAR W: Texts.Writer */
extern int32_t Out__done;     /* VAR done: BOOLEAN (0/1) */

#endif /* OUT_H */
