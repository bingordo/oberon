/* lower.c -- DESIGN.md 9.6: the C the back end must emit for each Oberon-07
 * statement form, written out by hand against the report's own example code.
 *
 * Each block carries the Oberon source as a comment and then the emitted C
 * verbatim, plus the assertions that decide whether the emission is correct.
 * Module prefix: a real module `M` emits `M__name`; here the fixture module is
 * `Golden`, so the emitted names are `Golden__log2`, ...
 *
 * The report's writing procedures call the Out module, and the emitted call is
 * `Out__Write(ch)` through the prototype in Out.h -- so this file exercises the
 * DESIGN 9.7 call shape as well as the statement shapes.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "Ob.h"      /* Texts__writer, Texts__Reset: the GUI-free writer Out writes to */
#include "Out.h"     /* the emitted Out interface (DESIGN 9.8) */
#include "harness.h"
#include "rt.h"

/* ==================================================================== */
/* 1. single-exit function result: log2                                   */
/* ==================================================================== */

/* PROCEDURE log2(x: INTEGER): INTEGER;
 *   VAR y: INTEGER;
 * BEGIN y := 0;
 *   WHILE x > 1 DO x := x DIV 2; INC(y) END;
 *   RETURN y
 * END log2;
 *
 * DESIGN 9.6/9.7: WHILE -> while, `DIV` -> rt_div, INC(y) -> y = y + 1, and the
 * trailing RETURN is the single assignment site of the result, so the emitted
 * body is a plain `return y;` at the end.  x is a value parameter: it is the
 * callee's copy, so mutating it is legal and invisible to the caller. */
int32_t Golden__log2(int32_t x) {
  int32_t y;
  y = 0;
  while (x > 1) {
    x = rt_div(x, 2);
    y = y + 1;
  }
  return y;
}

/* ==================================================================== */
/* 2. WriteInt: two REPEAT loops, a local CHAR array, CHR/ORD            */
/* ==================================================================== */

/* PROCEDURE WriteInt(x: INTEGER);
 *   VAR i, n: INTEGER; a: ARRAY 16 OF CHAR;
 * BEGIN
 *   IF x < 0 THEN Out.Write("-"); x := -x END;
 *   i := 0;
 *   REPEAT n := x MOD 10; x := x DIV 10; a[i] := CHR(n + ORD("0")); INC(i) UNTIL x = 0;
 *   REPEAT DEC(i); Out.Write(a[i]) UNTIL i = 0
 * END WriteInt;
 *
 * DESIGN 9.6: IF -> if/else, REPEAT S UNTIL c -> do { S } while (!(c)),
 * MOD -> rt_mod, DIV -> rt_div, CHR(n + ORD("0")) -> (char)(n + (int32_t)'0'),
 * INC/DEC -> i = i + 1 / i = i - 1, and the local ARRAY 16 OF CHAR stays a
 * char[16] (an uninitialised local array: only the digits written are read).
 * The report's negative case prints '-' and negates x first. */
void Golden__WriteInt(int32_t x) {
  int32_t i, n;
  char a[16];
  if (x < 0) {
    Out__Write('-');
    x = -x;
  }
  i = 0;
  do {
    n = rt_mod(x, 10);
    x = rt_div(x, 10);
    a[i] = (char)(n + (int32_t)'0');
    i = i + 1;
  } while (!(x == 0));
  do {
    i = i - 1;
    Out__Write(a[i]);
  } while (!(i == 0));
}

/* ==================================================================== */
/* 3. ReadInt: VAR parameter, & at the call site, `&` -> `&&`            */
/* ==================================================================== */

/* PROCEDURE ReadInt(VAR x: INTEGER);
 *   VAR i: INTEGER; ch: CHAR;
 * BEGIN i := 0; Read(ch);
 *   WHILE ("0" <= ch) & (ch <= "9") DO
 *     i := 10*i + (ORD(ch) - ORD("0")); Read(ch)
 *   END;
 *   x := i
 * END ReadInt;
 *
 * DESIGN 9.7: `VAR x: INTEGER` -> `int32_t *x`, call site passes `&x`.
 * DESIGN 9.5: `p & q` -> `(p && q)` (the operands are pure, so the
 * short-circuit form is the report's `&` on these), and ORD on a CHAR is the
 * unsigned byte value.  Read(ch) is the report's input predicate; the fixture
 * drives it from a fixed string, NUL-terminated so the loop stops. */
static const char *Golden__input = "";
static int32_t Golden__input_pos = 0;

void Golden__Read(char *ch) {
  char c = Golden__input[Golden__input_pos];
  if (c != 0) Golden__input_pos = Golden__input_pos + 1;
  *ch = c;
}

void Golden__read_from(const char *s) {
  Golden__input = s;
  Golden__input_pos = 0;
}

void Golden__ReadInt(int32_t *x) {
  int32_t i;
  char ch;
  i = 0;
  Golden__Read(&ch);
  while (('0' <= ch) && (ch <= '9')) {
    i = 10 * i + ((int32_t)(unsigned char)ch - (int32_t)'0');
    Golden__Read(&ch);
  }
  *x = i;
}

/* ==================================================================== */
/* 4. WHILE with ELSIF                                                    */
/* ==================================================================== */

/* VAR m, n: INTEGER;
 * WHILE m > n DO m := m - n ELSIF n > m DO n := n - m END
 *
 * DESIGN 9.6: "for (;;) { if (g1) {S1} else if (g2) {S2} else break; }", i.e.
 * repeat until no guard is true.  The procedure wrapper returns m so the
 * fixture can observe the result; the loop itself is the emitted text. */
int32_t Golden__subtract_until_equal(int32_t m, int32_t n) {
  for (;;) {
    if (m > n) {
      m = m - n;
    } else if (n > m) {
      n = n - m;
    } else
      break;
  }
  return m;
}

/* ==================================================================== */
/* 5. FOR, against the report's while-loop desugaring                     */
/* ==================================================================== */

/* PROCEDURE count(beg, end, inc: INTEGER): INTEGER; VAR v, n: INTEGER;
 * BEGIN n := 0;
 *   FOR v := beg TO end BY inc DO INC(n) END;
 *   RETURN n
 * END count;
 *
 * DESIGN 9.6 header:  for (v = beg; inc > 0 ? v <= end : v >= end; v += inc)
 * `BY` is required to be a non-zero integer *constant* (checker diagnostic 34),
 * so a real emission puts the constant in the header; it is a parameter here
 * only so the fixture can sweep the range shapes. */
int32_t Golden__for_count(int32_t beg, int32_t end, int32_t inc) {
  int32_t v, n;
  n = 0;
  for (v = beg; inc > 0 ? v <= end : v >= end; v += inc) {
    n = n + 1;
  }
  return n;
}

/* The emitted header's loop condition, taken on its own, so the inc == 0 case
 * can be compared with the report's while-form without running an infinite loop.
 * DESIGN 9.6 emits `inc > 0 ? v <= end : v >= end`, which is true at inc == 0
 * whenever v >= end; the report's while-form is false there because both of its
 * conjunctions are.  The checker rejects BY = 0 (diagnostic 34), so the emitted
 * header is only equivalent to the report for the programs that pass the
 * checker -- this case pins that. */
static int32_t Golden__for_header_cond(int32_t v, int32_t end, int32_t inc) {
  return inc > 0 ? v <= end : v >= end;
}

static int32_t Golden__for_report_cond(int32_t v, int32_t end, int32_t inc) {
  return ((inc > 0) && (v <= end)) || ((inc < 0) && (v >= end));
}

/* The report's own equivalence for the same statement:
 *   v := beg;
 *   WHILE (inc > 0) & (v <= end) OR (inc < 0) & (v >= end) DO S; v := v + inc END
 * run side by side with the emitted header so the two counts can be compared. */
int32_t Golden__for_count_report(int32_t beg, int32_t end, int32_t inc) {
  int32_t v, n;
  n = 0;
  v = beg;
  while (((inc > 0) && (v <= end)) || ((inc < 0) && (v >= end))) {
    n = n + 1;
    v = v + inc;
  }
  return n;
}

/* ==================================================================== */
/* 6. IF/ELSIF chain and REPEAT                                           */
/* ==================================================================== */

/* PROCEDURE sign(x: INTEGER): INTEGER; VAR s: INTEGER;
 * BEGIN
 *   IF x < 0 THEN s := -1 ELSIF x = 0 THEN s := 0 ELSIF x < 10 THEN s := 1 ELSE s := 2 END;
 *   RETURN s
 * END sign;
 * DESIGN 9.6: "if (…) { … } else if (…) { … } else { … }" */
int32_t Golden__sign(int32_t x) {
  int32_t s;
  if (x < 0) {
    s = -1;
  } else if (x == 0) {
    s = 0;
  } else if (x < 10) {
    s = 1;
  } else {
    s = 2;
  }
  return s;
}

/* PROCEDURE repeat_to_five(): INTEGER; VAR i: INTEGER;
 * BEGIN i := 0; REPEAT INC(i) UNTIL i = 5; RETURN i
 * END repeat_to_five;
 * DESIGN 9.6: REPEAT S UNTIL c -> do { S } while (!(c)).  The body runs before
 * the first test, which is what makes the loop below terminate after five
 * iterations and the second fixture after exactly one. */
int32_t Golden__repeat_to_five(void) {
  int32_t i;
  i = 0;
  do {
    i = i + 1;
  } while (!(i == 5));
  return i;
}

/* PROCEDURE repeat_once(): INTEGER; VAR i: INTEGER;
 * BEGIN i := 0; REPEAT INC(i) UNTIL i > 0; RETURN i
 * END repeat_once;   (* the guard is already true after one pass *) */
int32_t Golden__repeat_once(void) {
  int32_t i;
  i = 0;
  do {
    i = i + 1;
  } while (!(i > 0));
  return i;
}

/* ==================================================================== */
/* 7. CASE over an INTEGER scalar                                         */
/* ==================================================================== */

/* PROCEDURE arm(x: INTEGER): INTEGER; VAR a: INTEGER;
 * BEGIN a := -1;   (* the harness sentinel: the ELSE arm has an empty body *)
 *   CASE x OF
 *     0: a := 0
 *   | 1..3: a := 1
 *   | 4, 5: a := 2
 *   | ELSE
 *   END;
 *   RETURN a
 * END arm;
 *
 * DESIGN 9.6: "switch (c) with one case per label and ranges flattened to
 * consecutive labels"; the body-less default arm still needs its `break`, or
 * an unmatched value would fall out of the switch into whatever follows. */
int32_t Golden__case_arm(int32_t x) {
  int32_t a;
  a = -1;
  switch (x) {
    case 0:
      a = 0;
      break;
    case 1:
    case 2:
    case 3:
      a = 1;
      break;
    case 4:
    case 5:
      a = 2;
      break;
    default:
      break;
  }
  return a;
}

/* ==================================================================== */
/* the assertions                                                        */
/* ==================================================================== */

/* detail text for one INTEGER comparison; one buffer, one call per check */
static char lower_detail[192];
static const char *detail_i(const char *what, int32_t got, int32_t want) {
  snprintf(lower_detail, sizeof lower_detail, "%s: expected %d, got %d", what, (int)want, (int)got);
  return lower_detail;
}

static const char *detail_for(int32_t beg, int32_t end, int32_t inc, int32_t want) {
  snprintf(lower_detail, sizeof lower_detail,
           "for (%d,%d,%d): expected %d iteration(s); emitted header %d, report while-form %d",
           (int)beg, (int)end, (int)inc, (int)want, (int)Golden__for_count(beg, end, inc),
           (int)Golden__for_count_report(beg, end, inc));
  return lower_detail;
}

static void cases_runtime(void) {
  char b[8];

  check("rt-div-floor[-7 DIV 2 = -4, -7 MOD 2 = 1]",
        rt_div(-7, 2) == -4 && rt_mod(-7, 2) == 1,
        detail_i("-7 DIV/MOD 2", rt_div(-7, 2), -4));
  check("rt-div-floor[7 DIV -2 = -4, 7 MOD -2 = -1]",
        rt_div(7, -2) == -4 && rt_mod(7, -2) == -1,
        detail_i("7 DIV -2", rt_div(7, -2), -4));
  check("rt-div-y=-1[-7 DIV -1 = 7, -7 MOD -1 = 0, INT32_MIN DIV -1 = INT32_MIN, no idiv trap]",
        rt_div(-7, -1) == 7 && rt_mod(-7, -1) == 0 && rt_div(INT32_MIN, -1) == INT32_MIN,
        detail_i("-7 DIV -1", rt_div(-7, -1), 7));
  /* The report defines ASR as `x DIV 2^n` (R22), so a count of 33 is 0, not a
   * masked no-op. */
  check("rt-asr-portable[-8 ASR 1 = -4, -1 ASR 31 = -1, 33 ASR 33 = 0 (x DIV 2^n)]",
        rt_asr(-8, 1) == -4 && rt_asr(-1, 31) == -1 && rt_asr(33, 33) == 0,
        detail_i("-8 ASR 1", rt_asr(-8, 1), -4));
  /* LSL is `x * 2^n`, so multiplying by 2^32 wraps to zero rather than
   * shifting by zero (R22). */
  check("rt-lsl[-1 LSL 31 = INT32_MIN, 1 LSL 32 = 0 (x * 2^n wraps)]",
        rt_lsl(-1, 31) == INT32_MIN && rt_lsl(1, 32) == 0,
        detail_i("-1 LSL 31", rt_lsl(-1, 31), INT32_MIN));
  check("rt-ror[0x12345678 ROR 8 = 0x78123456, ROR 0 = identity]",
        rt_ror(0x12345678u, 8) == 0x78123456u && rt_ror(0x12345678u, 0) == 0x12345678u,
        "ROR wrong");
  check("rt-set+rt-in[{1..3} has 1,2,3 and not 4; {5..1} is empty]",
        rt_in(rt_set(1, 3), 1) == 1 && rt_in(rt_set(1, 3), 3) == 1 &&
            rt_in(rt_set(1, 3), 4) == 0 && rt_set(5, 1) == 0u,
        "SET element or empty range wrong");
  rt_strcopy(b, 4, "ab");
  check("rt-strcopy-zero-fill[\"ab\" into char[4] zero-fills the tail]",
        b[0] == 'a' && b[1] == 'b' && b[2] == 0 && b[3] == 0, "tail not zero-filled");
  rt_strcopy(b, 4, "abc");
  check("rt-strcopy-exact-fit[\"abc\" into char[4] keeps the NUL at 3]",
        rt_streq(b, "abc") && b[3] == 0, "exact fit wrong");
  {
    /* The source fills the destination, as it does for a same-length ARRAY OF
     * CHAR assignment (DESIGN 8.2 makes array equality structural, so that is
     * the only other `rt_strcopy` case; a string literal is required to satisfy
     * len(src) < n by diagnostic 9).  Then there is no NUL in the copy -- the
     * report's fixed-length semantics, not a bug. */
    char full[4];
    rt_strcopy(full, 4, "abcd");
    check("rt-strcopy-full-length-no-terminator[char[4] := \"abcd\" copies 4 chars]",
          full[0] == 'a' && full[1] == 'b' && full[2] == 'c' && full[3] == 'd',
          "full-length copy wrong");
  }
  check("rt-strcmp[\"abc\" < \"abd\" and \"abc\" = \"abc\"]",
        rt_strlt("abc", "abd") && rt_streq("abc", "abc"), "comparison wrong");
}

static void cases_shifts(void) {
  int32_t n, x;
  int32_t bad_lsl = 0, bad_asr = 0, bad_ror = 0;
  for (n = 0; n < 32; n = n + 1) {
    for (x = -300; x <= 300; x = x + 7) {
      if (rt_lsl(x, n) != (int32_t)((uint32_t)x * (uint32_t)(1u << n))) bad_lsl = bad_lsl + 1;
      /* ASR(x,n) = x DIV 2^n only for n <= 30: 2^31 is not an INTEGER. */
      if (n <= 30 && rt_asr(x, n) != rt_div(x, (int32_t)(1u << n))) bad_asr = bad_asr + 1;
      {
        uint32_t r = (uint32_t)x;
        int32_t k;
        for (k = 0; k < n; k = k + 1) r = (r >> 1) | (r << 31); /* one-bit ROR, n times */
        if ((uint32_t)rt_ror((int32_t)x, n) != r) bad_ror = bad_ror + 1;
      }
    }
  }
  check("rt-lsl-brute-force[x in -300..300 step 7, n in 0..31: == x * 2^n mod 2^32]",
        bad_lsl == 0, detail_i("mismatches", bad_lsl, 0));
  check("rt-asr-brute-force[== x DIV 2^n for every n in 0..30]",
        bad_asr == 0, detail_i("mismatches", bad_asr, 0));
  check("rt-ror-brute-force[== n single-bit rotations, n in 0..31]",
        bad_ror == 0, detail_i("mismatches", bad_ror, 0));
}

static void cases_log2(void) {
  check("log2-1->0", Golden__log2(1) == 0, detail_i("log2(1)", Golden__log2(1), 0));
  check("log2-2->1", Golden__log2(2) == 1, detail_i("log2(2)", Golden__log2(2), 1));
  check("log2-1024->10", Golden__log2(1024) == 10, detail_i("log2(1024)", Golden__log2(1024), 10));
  check("log2-1000->9", Golden__log2(1000) == 9, detail_i("log2(1000)", Golden__log2(1000), 9));
}

static void case_writeint(const char *name, int32_t x, const char *expected) {
  Texts__Reset(&Texts__writer);
  Golden__WriteInt(x); /* -> Out.Write -> Out__Write into Texts__writer */
  check_str(name, Texts__writer.buf, expected);
}

static void cases_writeint(void) {
  case_writeint("WriteInt-0[\"0\"]", 0, "0");
  case_writeint("WriteInt-7[\"7\"]", 7, "7");
  case_writeint("WriteInt-12345[\"12345\"]", 12345, "12345");
  case_writeint("WriteInt--7[\"-7\"]", -7, "-7");
  case_writeint("WriteInt--12345[\"-12345\"]", -12345, "-12345");
}

static void case_readint(const char *name, const char *input, int32_t expected) {
  int32_t x;
  x = -12345; /* the VAR actual must be written by the callee */
  Golden__read_from(input);
  Golden__ReadInt(&x);
  check(name, x == expected, detail_i(input, x, expected));
}

static void cases_readint(void) {
  case_readint("ReadInt-\"123\"->123", "123", 123);
  case_readint("ReadInt-\"\"->0", "", 0);
  case_readint("ReadInt-\"0\"->0", "0", 0);
  case_readint("ReadInt-\"42x\"->42 (stops at the first non-digit)", "42x", 42);
}

static void cases_while_elsif(void) {
  check("while-elsif-gcd[12,18 -> 6]",
        Golden__subtract_until_equal(12, 18) == 6,
        detail_i("12,18", Golden__subtract_until_equal(12, 18), 6));
  check("while-elsif-gcd[5,5 -> 5, no iteration at all]",
        Golden__subtract_until_equal(5, 5) == 5,
        detail_i("5,5", Golden__subtract_until_equal(5, 5), 5));
  check("while-elsif-gcd[100,75 -> 25]",
        Golden__subtract_until_equal(100, 75) == 25,
        detail_i("100,75", Golden__subtract_until_equal(100, 75), 25));
}

static void case_for(const char *name, int32_t beg, int32_t end, int32_t inc, int32_t want) {
  check(name,
        Golden__for_count(beg, end, inc) == want && Golden__for_count_report(beg, end, inc) == want,
        detail_for(beg, end, inc, want));
}

static void cases_for(void) {
  case_for("for-(0,10,+3)->4, equals the report's while-form", 0, 10, 3, 4);
  case_for("for-(10,0,-3)->4, equals the report's while-form", 10, 0, -3, 4);
  case_for("for-(5,5,+1)->1, equals the report's while-form", 5, 5, 1, 1);
  case_for("for-(5,5,-1)->1, equals the report's while-form", 5, 5, -1, 1);
  case_for("for-(1,10,-1)->0 (empty), equals the report's while-form", 1, 10, -1, 0);
  /* inc == 0 is rejected by the checker (diagnostic 34); if it were not, the
   * emitted header would not terminate where the report's form executes zero
   * times.  Asserted as an observed, documented divergence. */
  check("for-BY=0-divergence[emitted header condition true, report while-form false; unreachable: diagnostic 34 rejects BY = 0]",
        Golden__for_header_cond(5, 0, 0) == 1 && Golden__for_report_cond(5, 0, 0) == 0,
        "v = 5, end = 0, inc = 0: emitted header and report while-form should disagree here");
}

static void cases_if_repeat(void) {
  check("if-elsif-chain[-3->-1, 0->0, 5->1, 50->2]",
        Golden__sign(-3) == -1 && Golden__sign(0) == 0 && Golden__sign(5) == 1 &&
            Golden__sign(50) == 2,
        detail_i("sign(50)", Golden__sign(50), 2));
  check("repeat-body-then-test[i = 5]",
        Golden__repeat_to_five() == 5,
        detail_i("repeat_to_five()", Golden__repeat_to_five(), 5));
  check("repeat-do-while-tests-after-body[i = 1, guard i > 0 true at once]",
        Golden__repeat_once() == 1,
        detail_i("repeat_once()", Golden__repeat_once(), 1));
}

static void cases_case(void) {
  int32_t arms[6];
  char seen[192];
  int32_t i, ok;
  static const int32_t values[6] = {0, 1, 3, 4, 5, 6};
  for (i = 0; i < 6; i = i + 1) arms[i] = Golden__case_arm(values[i]);
  ok = arms[0] == 0 && arms[1] == 1 && arms[2] == 1 && arms[3] == 2 && arms[4] == 2 && arms[5] == -1;
  snprintf(seen, sizeof seen, "arm(v) for v = 0,1,3,4,5,6 came out %d,%d,%d,%d,%d,%d (want 0,1,1,2,2,-1)",
           (int)arms[0], (int)arms[1], (int)arms[2], (int)arms[3], (int)arms[4], (int)arms[5]);
  check("case-arm-vector[0->0, 1..3->1, 4,5->2, ELSE body-less->-1]", ok, seen);
}

/* ==================================================================== */
/* 8. CHAR array comparison and copy (DESIGN 9.5/9.6, finding R14)        */
/* ==================================================================== */

/* `a < b` on CHAR arrays emits rt_charcmp(a, na, b, nb): bounded by the shorter
 * operand, stopping at a NUL in either.  It may not assume termination, because
 * a same-length ARRAY OF CHAR copy (rt_charcopy) implies no terminator.  This
 * group checks the algorithm against `strcmp` where the operands *are*
 * terminated, and against filled operands -- with a sentinel, and with a
 * sanitizer redzone, right behind them -- where `strcmp` would answer wrongly. */

static int32_t sgn(int32_t v) { return (v > 0) - (v < 0); }

typedef struct {
  const char *p;
  int32_t n;
  const char *label;
} Operand;

/* filled, unterminated operands */
static const char op_abc[3] = {'a', 'b', 'c'};
static const char op_abd[3] = {'a', 'b', 'd'};
static const char op_abce[4] = {'a', 'b', 'c', 'e'};
static const char op_ab[2] = {'a', 'b'};
static const char op_ab0[3] = {'a', 'b', 0};
static const char op_ax[3] = {'a', 0, 'x'};
static const char op_abcd[4] = {'a', 'b', 'c', 'd'};

static const Operand cmp_operands[] = {
    {"abc", 4, "\"abc\""},
    {"abd", 4, "\"abd\""},
    {"ab", 3, "\"ab\""},
    {"", 1, "\"\""},
    {"a", 2, "\"a\""},
    {op_abc, 3, "{a,b,c}"},
    {op_abd, 3, "{a,b,d}"},
    {op_ab, 2, "{a,b}"},
    {op_ab0, 3, "{a,b,0}"},
    {op_ax, 3, "{a,0,x}"},
    {op_abcd, 4, "{a,b,c,d}"}};
#define CMP_OPERANDS ((int32_t)(sizeof cmp_operands / sizeof cmp_operands[0]))

/* two 4-byte operands laid out adjacently, with a sentinel byte right after the
 * first one: reading index 4 of the first would see 0xFF, not a terminator */
static struct {
  char a[4];
  char sentinel;
  char b[4];
  char zero;
} adj_pair;

static const char *detail_cmp(const Operand *x, const Operand *y) {
  snprintf(lower_detail, sizeof lower_detail, "%s vs %s: rt_charcmp = %d", x->label, y->label,
           (int)rt_charcmp(x->p, x->n, y->p, y->n));
  return lower_detail;
}

static void cases_charcmp(void) {
  const Operand *O = cmp_operands;

  /* 1. agreement with strcmp's sign where both operands are NUL-terminated */
  check("charcmp-strcmp-agreement[(abc,abd)>0, (abc,abc)=0, (ab,abc)<0, (,)=0, (,a)<0]",
        sgn(rt_charcmp("abc", 4, "abd", 4)) == sgn(strcmp("abc", "abd")) &&
            rt_charcmp("abc", 4, "abc", 4) == 0 &&
            sgn(rt_charcmp("ab", 3, "abc", 4)) == sgn(strcmp("ab", "abc")) &&
            rt_charcmp("", 1, "", 1) == 0 && sgn(rt_charcmp("", 1, "a", 2)) == sgn(strcmp("", "a")),
        "the bounded comparison disagrees with strcmp on terminated operands");

  /* the same agreement, exhaustively, over the five NUL-terminated operands.
   * For two operands that are both well-formed strings the algorithm can never
   * reach its length fallback (each has its NUL inside its own bound), so it
   * must agree with `strcmp` on every pair -- which is the property that makes
   * it a drop-in replacement for the relational operators. */
  {
    int32_t i, j, bad = 0;
    for (i = 0; i <= 4; i = i + 1) {
      for (j = 0; j <= 4; j = j + 1) {
        if (sgn(rt_charcmp(O[i].p, O[i].n, O[j].p, O[j].n)) != sgn(strcmp(O[i].p, O[j].p)))
          bad = bad + 1;
      }
    }
    check("charcmp-strcmp-agreement-exhaustive[all 25 ordered pairs of NUL-terminated operands]",
          bad == 0, detail_i("sign disagreements with strcmp", bad, 0));
  }

  /* 2. two filled arrays of equal length with no NUL inside */
  check("charcmp-filled-equal-length-no-NUL[{a,b,c} vs {a,b,d} < 0]",
        rt_charcmp(op_abc, 3, op_abd, 3) < 0, detail_cmp(&O[5], &O[6]));
  check("charcmp-filled-identical-no-NUL[{a,b,c} vs {a,b,c} = 0]",
        rt_charcmp(op_abc, 3, op_abc, 3) == 0, detail_cmp(&O[5], &O[5]));

  /* 3. a filled array (2 bytes, no NUL) against the same content with a NUL (3
   *    bytes): the shipped runtime compares *content* and treats the byte after
   *    an operand's length as NUL, so these are equal - which is also what
   *    strcmp says about two terminated operands with the same content. The
   *    earlier expectations here came from a private copy of rt_charcmp that had
   *    drifted; including the emitted header (DESIGN §14) is what caught it. */
  check("charcmp-filled-vs-same-content-terminated[{a,b}(2) == {a,b,0}(3)]",
        rt_charcmp(op_ab, 2, op_ab0, 3) == 0, detail_cmp(&O[7], &O[8]));
  check("charcmp-same-content-terminated-vs-filled[{a,b,0}(3) == {a,b}(2)]",
        rt_charcmp(op_ab0, 3, op_ab, 2) == 0, detail_cmp(&O[8], &O[7]));
  /* …but a longer *content* is greater, whatever the lengths are. */
  check("charcmp-longer-content-is-greater[{a,b,c}(3) > {a,b,0}(3)]",
        rt_charcmp(op_abc, 3, op_ab0, 3) > 0, detail_cmp(&O[5], &O[8]));

  /* 4. a NUL inside one operand: decided at the second byte, third byte unused */
  check("charcmp-nul-inside-operand[{a,0,x} vs {a,b,0} < 0 at the second byte]",
        rt_charcmp(op_ax, 3, op_ab0, 3) < 0, detail_cmp(&O[9], &O[8]));
  {
    static const char op_ay[3] = {'a', 0, 'y'};
    static const char op_abz[3] = {'a', 'b', 'z'};
    check("charcmp-nul-inside-ignores-later-bytes[changing the third byte changes nothing]",
          rt_charcmp(op_ay, 3, op_abz, 3) == rt_charcmp(op_ax, 3, op_ab0, 3),
          "the comparison reached past the NUL at byte 1");
  }

  /* 5. antisymmetry and reflexivity over every ordered pair above; `nz` guards
   *    against a degenerate implementation that answers 0 everywhere and would
   *    satisfy antisymmetry vacuously */
  {
    int32_t i, j, bad = 0, nz = 0;
    for (i = 0; i < CMP_OPERANDS; i = i + 1) {
      if (rt_charcmp(O[i].p, O[i].n, O[i].p, O[i].n) != 0) bad = bad + 1;
      for (j = 0; j < CMP_OPERANDS; j = j + 1) {
        int32_t ij = rt_charcmp(O[i].p, O[i].n, O[j].p, O[j].n);
        if (ij != 0) nz = nz + 1;
        if (sgn(ij) != -sgn(rt_charcmp(O[j].p, O[j].n, O[i].p, O[i].n))) bad = bad + 1;
      }
    }
    check("charcmp-antisymmetry[all 121 ordered pairs: sign(a,b) = -sign(b,a), cmp(x,x) = 0]",
          bad == 0 && nz > 0,
          detail_i("antisymmetry violations (and non-zero pairs, which must not be 0)", bad, 0));
    check("charcmp-order-is-not-degenerate[some of the 121 pairs do compare unequal]",
          nz > 0, detail_i("non-zero comparisons", nz, 121));
  }

  /* 6. the boundary case from R14: adjacent operands, sentinel after the first.
   *    An unbounded comparison would reach a[4] = 0xFF against b[4] = 0 and
   *    answer > 0; the bounded one must answer 0 without reading either. */
  adj_pair.sentinel = (char)0xFF;
  rt_charcopy(adj_pair.a, 4, op_abcd, 4);
  rt_charcopy(adj_pair.b, 4, op_abcd, 4);
  check("charcmp-adjacent-struct-sentinel[= 0, where an unbounded compare would read 0xFF and answer > 0]",
        rt_charcmp(adj_pair.a, 4, adj_pair.b, 4) == 0 &&
            (unsigned char)adj_pair.sentinel == 0xFFu &&
            (int32_t)(unsigned char)adj_pair.sentinel - (int32_t)(unsigned char)adj_pair.zero > 0,
        "the sentinel after a[3] leaked into the answer");
  rt_charcopy(adj_pair.b, 4, op_abce, 4);
  check("charcmp-adjacent-struct-differ-in-last-byte[{a,b,c,d} vs {a,b,c,e} < 0]",
        rt_charcmp(adj_pair.a, 4, adj_pair.b, 4) < 0, "differing last byte ignored");

  /* 7. the same property with the operands in separate 4-byte heap blocks: an
   *    unbounded comparison would read the byte after a block, which under
   *    -fsanitize=address is a redzone hit.  The sanitized build of this file
   *    (run.sh) is therefore the detector for "never reads past either
   *    operand", which the in-struct sentinel cannot show by itself. */
  {
    char *ha = malloc(4);
    char *hb = malloc(4);
    if (ha == NULL || hb == NULL) {
      check("charcmp-heap-differ-in-last-byte[< 0; ASan proves no read past either block]", 0,
            "malloc failed");
      check("charcmp-heap-identical[= 0; ASan proves no redzone read at index 4]", 0,
            "malloc failed");
    } else {
      rt_charcopy(ha, 4, op_abcd, 4);
      rt_charcopy(hb, 4, op_abce, 4);
      check("charcmp-heap-differ-in-last-byte[< 0; ASan proves no read past either block]",
            rt_charcmp(ha, 4, hb, 4) < 0, "heap operands compared wrong");
      rt_charcopy(hb, 4, op_abcd, 4);
      check("charcmp-heap-identical[= 0; ASan proves no redzone read at index 4]",
            rt_charcmp(ha, 4, hb, 4) == 0, "identical heap operands compared non-zero");
    }
    free(ha);
    free(hb);
  }

  /* 8. DESIGN 9.6's two copies: the string-literal form zero-pads, the
   *    array-to-array form of equal length appends nothing, and the comparison
   *    of a full-length copy is decided by rt_charcmp. */
  {
    char s[6];
    char d[4];
    char same[4];
    char other[4];
    int32_t i;

    rt_strcopy(s, 6, "abc");
    check("rt-strcopy-literal-zero-pads[char[6] := \"abc\" is abc + 3 zeros]",
          s[0] == 'a' && s[1] == 'b' && s[2] == 'c' && s[3] == 0 && s[4] == 0 && s[5] == 0,
          "string-literal copy did not zero-pad");

    rt_charcopy(d, 4, op_abcd, 4);
    rt_charcopy(same, 4, op_abcd, 4);
    rt_charcopy(other, 4, op_abce, 4);
    check("rt-charcopy-equal-length-no-terminator[char[4] := {a,b,c,d} copies 4 bytes, appends nothing]",
          d[0] == 'a' && d[1] == 'b' && d[2] == 'c' && d[3] == 'd' && d[0] != 0 && d[1] != 0 &&
              d[2] != 0 && d[3] != 0,
          "array-to-array copy added or lost bytes");

    check("rt-charcopy-full-length-then-charcmp[copy = source, copy < {a,b,c,e}, no crash]",
          rt_charcmp(d, 4, same, 4) == 0 && rt_charcmp(d, 4, other, 4) < 0,
          "bounded comparison of a full-length copy wrong");

    /* the min(): the emitter only ever emits m == n, the bound keeps the helper
     * total, and the destination tail is left untouched */
    for (i = 0; i < 6; i = i + 1) s[i] = (char)0x7F;
    rt_charcopy(s, 6, op_abc, 3);
    check("rt-charcopy-shorter-source-bounded[min(6,3) bytes copied, tail untouched]",
          s[0] == 'a' && s[1] == 'b' && s[2] == 'c' && s[3] == 0x7F && s[4] == 0x7F && s[5] == 0x7F,
          "rt_charcopy(n > m) wrote outside min(n,m)");
  }
}

void lower_run_tests(void) {
  cases_runtime();
  cases_shifts();
  cases_log2();
  cases_writeint();
  cases_readint();
  cases_while_elsif();
  cases_for();
  cases_if_repeat();
  cases_case();
  cases_charcmp(); /* appended: the 47 case lines above keep their exact text and order */
}
