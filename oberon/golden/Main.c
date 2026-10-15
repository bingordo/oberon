/* Main.c -- the "program" of DESIGN 9.8: the emitted prologue activates the
 * modules the program imports (here Out, whose body stands up the writer), and
 * the host then drives the module both through its exported prototypes and
 * through the command table of DESIGN 9.7. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "Ob.h"
#include "Out.h"
#include "harness.h"

/* lower.c is the DESIGN 9.6 fixture; the emitter would write this prototype
 * into Golden.h, next to Golden's exported procedures. */
void lower_run_tests(void);

/* constexpr_check.c verifies the constant-expression table against the back
 * end's own semantics (DESIGN 8.3's folder-vs-emitted-C claim). */
void constexpr_check_tests(void);

int main(void) {
  const ObCommand *entry;
  int32_t ok;

  /* ---- the program prologue: load the imported modules ---- */
  Out__load(); /* Out__init() + ob_register_commands("Out", Out__commands) */
  Out__init(); /* idempotent: the module body may be activated twice */

  /* DESIGN §7/9.8: an exported variable is read-only for the importing side.
   * These two reads are the only appearances of Out__W / Out__done outside
   * Out.c in the whole fixture. */
  check("module-body-sets-exported-vars[Out.done = 1, Out.W = the host writer]",
        Out__done == 1 && Out__W == &Texts__writer,
        "the module body did not set the exported variables");

  /* ---- DESIGN 9.6: the statement lowerings ---- */
  lower_run_tests();

  /* ---- DESIGN 9.7: dispatch through the command table ---- */

  Texts__Reset(&Texts__writer);
  ok = ob_run_command("Out", "WriteLn"); /* a command really is void (void) */
  check("command-WriteLn-found-and-ran[returns 1 and appends one newline]",
        ok == 1 && Texts__writer.len == 1 && Texts__writer.buf[0] == '\n',
        "ob_run_command(\"Out\",\"WriteLn\") did not append exactly one newline");

  ok = ob_run_command("Out", "NoSuchCommand");
  check("command-unknown-name-fails[returns 0 and writes nothing]",
        ok == 0 && Texts__writer.len == 1,
        "an unregistered command name did not fail cleanly");

  ok = ob_run_command("NoSuchModule", "WriteLn");
  check("command-unknown-module-fails[returns 0]", ok == 0,
        "an unregistered module did not fail cleanly");

  /* The table entry for a command with parameters cannot be *called* through
   * void (*)(void); the host that knows the real Oberon signature finds the
   * entry and converts the stored pointer back, which C11 6.3.2.3p8 makes
   * well-defined (and the round trip must compare equal, which is what the
   * first assertion below checks). */
  entry = ob_find_command("Out", "WriteInt");
  check("command-table-entry-WriteInt-present[address round-trips]",
        entry != NULL && strcmp(entry->name, "WriteInt") == 0 &&
            entry->fn == (void (*)(void))Out__WriteInt,
        "no WriteInt entry, or it does not point at Out__WriteInt");
  if (entry != NULL) {
    void (*writeint)(int32_t, int32_t) = (void (*)(int32_t, int32_t))entry->fn;
    Texts__Reset(&Texts__writer);
    writeint(12345, 8); /* Out.WriteInt(12345, 8) */
    check("command-WriteInt-dispatch-output[field 8 -> \"   12345 \"]",
          strcmp(Texts__writer.buf, "   12345 ") == 0 && Texts__writer.len == 9,
          Texts__writer.buf);
  } else {
    check("command-WriteInt-dispatch-output[field 8 -> \"   12345 \"]", 0,
          "no WriteInt entry to dispatch");
  }

  /* ---- DESIGN 9.8: the exported prototypes are callable directly ---- */

  Texts__Reset(&Texts__writer);
  Out__Write('X');
  Out__WriteInt(-42, 6);
  Out__WriteLn();
  check("exported-prototype-direct-call[\"X\" + WriteInt(-42,6) + WriteLn = \"X-   42 \\n\"]",
        strcmp(Texts__writer.buf, "X-   42 \n") == 0 && Texts__writer.len == 9,
        Texts__writer.buf);
  printf("captured writer buffer: [%s]  (len = %d)\n", Texts__writer.buf, (int)Texts__writer.len);

  /* ---- DESIGN 8.3: the folder and the emitted C compute the same values ---- */
  constexpr_check_tests();

  harness_summary("golden");
  return harness_failures() == 0 ? 0 : 1;
}
