/* Out.c -- module Out, hand-written in the emitted shape (DESIGN 9.7/9.8).
 *
 * Oberon (report §10 examples, the GUI-free part):
 *
 *   MODULE Out;
 *     IMPORT Texts;
 *     VAR W: Texts.Writer; done: BOOLEAN;
 *
 *     PROCEDURE Write*(ch: CHAR);
 *     BEGIN Texts.Write(W, ch)
 *     END Write;
 *
 *     PROCEDURE WriteLn*;
 *     BEGIN Texts.WriteLn(W); Texts.Append(Oberon.Log, W.buf)
 *     END WriteLn;
 *
 *     PROCEDURE WriteInt*(x, n: INTEGER);
 *       VAR i: INTEGER; a: ARRAY 16 OF CHAR;
 *     BEGIN i := 0;
 *       IF x < 0 THEN Write("-"); x := -x; DEC(n) END;
 *       REPEAT a[i] := CHR(x MOD 10 + ORD("0")); x := x DIV 10; INC(i) UNTIL x = 0;
 *       WHILE n > i DO Write(" "); DEC(n) END;
 *       WHILE i > 0 DO DEC(i); Write(a[i]) END;
 *       Write(" ")
 *     END WriteInt;
 *   BEGIN Texts.OpenWriter(W); done := TRUE
 *   END Out.
 */
#include "Out.h"
#include "rt.h"
#include <stddef.h>

/* ------------------------------------------------------------------ */
/* module-level objects                                                 */
/* ------------------------------------------------------------------ */

/* Exported variables: the definitions live here, and only here.  DESIGN §7
 * makes them read-only for importers, but see Out.h: C has no way to express
 * "assignable in the defining module, read-only elsewhere" for a variable, so
 * this stays a front-end rule. */
Texts__Writer *Out__W;
int32_t Out__done;

/* ------------------------------------------------------------------ */
/* exported procedures                                                  */
/* ------------------------------------------------------------------ */

void Out__Write(char ch) { Texts__Write(Out__W, ch); }

void Out__WriteLn(void) { Texts__WriteLn(Out__W); }

void Out__WriteInt(int32_t x, int32_t n) {
  int32_t i;
  char a[16];
  i = 0;
  if (x < 0) {
    Out__Write('-');
    x = -x;
    n = n - 1;
  }
  do {
    a[i] = (char)(rt_mod(x, 10) + (int32_t)'0');
    x = rt_div(x, 10);
    i = i + 1;
  } while (!(x == 0));
  while (n > i) {
    Out__Write(' ');
    n = n - 1;
  }
  while (i > 0) {
    i = i - 1;
    Out__Write(a[i]);
  }
  Out__Write(' ');
}

/* ------------------------------------------------------------------ */
/* the module body and the loader                                       */
/* ------------------------------------------------------------------ */

void Out__init(void) {
  Out__W = &Texts__writer; /* Texts.OpenWriter(W) */
  Out__done = 1;
}

/* ------------------------------------------------------------------ */
/* command table (DESIGN 9.7)                                           */
/* ------------------------------------------------------------------ */

/* A procedure that references an enclosing frame gets a hidden first
 * parameter, `void *<Module>__static_link`, and so has type void (*)(void *);
 * such a procedure is *not* command-shaped, and its address may not be stored
 * in the table below.  Out__linked_probe is exactly that case, and the three
 * static assertions pin every type involved, so the casts in the table cannot
 * silently hide a hidden argument. */
void Out__linked_probe(void *Out__static_link) { (void)Out__static_link; }

_Static_assert(_Generic(&Out__Write, void (*)(char) : 1, default : 0),
               "Out__Write must be void (*)(char)");
_Static_assert(_Generic(&Out__WriteLn, void (*)(void) : 1, default : 0),
               "Out__WriteLn must be void (*)(void)");
_Static_assert(_Generic(&Out__linked_probe, void (*)(void *) : 1, default : 0),
               "a link-taking procedure must be void (*)(void *)");

/* The table stores command pointers.  The cast is valid for these three
 * procedures because each is a module-level procedure of an outermost module:
 * it references no enclosing frame (there is none), so it has no hidden static
 * link, and its only mismatch with void (*)(void) is the declared parameters.
 * Converting a function pointer to another function pointer type and *calling it
 * through the converted type with a signature the declaration does not have*
 * would be undefined; `ob_run_command` therefore only ever invokes the
 * entries whose real signature is `void (void)` (WriteLn).  `Write` and
 * `WriteInt` are registered because DESIGN 9.7's example table registers
 * `Write`; the host reaches them through the typed round-trip demonstrated in
 * Main.c. */
static const ObCommand Out__commands[] = {
    {"Write", (void (*)(void))Out__Write},
    {"WriteInt", (void (*)(void))Out__WriteInt},
    {"WriteLn", (void (*)(void))Out__WriteLn},
    {NULL, NULL}};

/* The loader: activate the module body, then publish its commands under the
 * module's name.  A program that imports Out calls this once before anything
 * else (that is what the emitted program's prologue does). */
void Out__load(void) {
  Out__init();
  ob_register_commands("Out", Out__commands);
}
