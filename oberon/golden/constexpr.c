/* constexpr.c -- writes constexpr.tsv: the C values of the Oberon-07 constant
 * expressions the front end's folder (oberon/src/const_eval.rs) folds.
 *
 * Why: DESIGN.md §8.3 claims the folder and the emitted C compute the same
 * values, because both implement the report's floor DIV/MOD, the mod-32 shift
 * counts and the set operations the same way.  This program is the C half of
 * that claim: one row per expression, each value computed by the *runtime
 * helpers the back end emits* (rt_div, rt_mod, rt_lsl, rt_asr, rt_ror, rt_set,
 * rt_in, plus rt_floor/rt_flt/rt_ord for FLOOR/FLT/ORD), never by a literal and
 * never by an algorithm the back end does not use.
 *
 * Build and run: `bash run.sh` (or: gcc -std=c11 -O2 -fwrapv -Wall -Wextra
 * -Werror -o constexpr constexpr.c rt.c && ./constexpr).
 *
 * Format: a leading '#' comment block, then one data row per expression,
 * `<expression><TAB><decimal value>`, in the order the expressions are listed
 * in const_expr_rows[] below.
 *
 * Value conventions (shared with the Rust reader, see const_eval.rs's
 * tests::agrees_with_the_emitted_c):
 *   INTEGER, BOOLEAN : signed decimal of the int32_t/two's-complement value
 *   SET              : the 32-bit pattern as an unsigned decimal (0..4294967295)
 *   CHAR             : the ordinal, as for INTEGER
 *   REAL             : truncated toward zero
 * The expression text is written exactly as it will be parsed back: signs that
 * bind a whole term stay signs (`-7 DIV 3` is `-(7 DIV 3)`, DESIGN §8.3), and
 * where a sign would not be grammatical in a factor the operand is spelled as a
 * subtraction in parentheses (see the notes in the header emitted below).
 */
#include <stdint.h>
#include <stdio.h>

#include "rt.h"

/* ==================================================================== */
/* one small function per expression: the C the back end would emit      */
/* ==================================================================== */

/* 2*100 - 1              ==>  2 * 100 - 1 */
static int64_t v_2_times_100_minus_1(void) { return (int64_t)(2 * 100 - 1); }

/* 100H                   ==>  0x100 (a hexadecimal literal is a bit pattern) */
static int64_t v_100H(void) { return (int64_t)0x100; }

/* 0FFFFFFFFH             ==>  (int32_t)0xFFFFFFFFu, the 32-bit pattern as INTEGER */
static int64_t v_0FFFFFFFFH(void) { return (int64_t)(int32_t)0xFFFFFFFFu; }

/* 7FFFFFFFH              ==>  (int32_t)0x7FFFFFFFu */
static int64_t v_7FFFFFFFH(void) { return (int64_t)(int32_t)0x7FFFFFFFu; }

/* 7 DIV 3                ==>  rt_div(7, 3) */
static int64_t v_7_div_3(void) { return (int64_t)rt_div(7, 3); }

/* 7 MOD 3                ==>  rt_mod(7, 3) */
static int64_t v_7_mod_3(void) { return (int64_t)rt_mod(7, 3); }

/* -7 DIV 3               ==>  -(rt_div(7, 3))   [the sign binds the whole term] */
static int64_t v_m7_div_3(void) { return (int64_t)(-rt_div(7, 3)); }

/* -7 MOD 3               ==>  -(rt_mod(7, 3)) */
static int64_t v_m7_mod_3(void) { return (int64_t)(-rt_mod(7, 3)); }

/* 7 DIV (0 - 3)          ==>  rt_div(7, 0 - 3) */
static int64_t v_7_div_m3(void) { return (int64_t)rt_div(7, 0 - 3); }

/* 7 MOD (0 - 3)          ==>  rt_mod(7, 0 - 3) */
static int64_t v_7_mod_m3(void) { return (int64_t)rt_mod(7, 0 - 3); }

/* (0 - 7) DIV 3          ==>  rt_div(0 - 7, 3) */
static int64_t v_m7_p_div_3(void) { return (int64_t)rt_div(0 - 7, 3); }

/* (0 - 7) MOD 3          ==>  rt_mod(0 - 7, 3) */
static int64_t v_m7_p_mod_3(void) { return (int64_t)rt_mod(0 - 7, 3); }

/* (0 - 7) DIV (0 - 3)    ==>  rt_div(0 - 7, 0 - 3) */
static int64_t v_m7_p_div_m3(void) { return (int64_t)rt_div(0 - 7, 0 - 3); }

/* (0 - 7) MOD (0 - 3)    ==>  rt_mod(0 - 7, 0 - 3) */
static int64_t v_m7_p_mod_m3(void) { return (int64_t)rt_mod(0 - 7, 0 - 3); }

/* 7 / 3                  ==>  rt_div(7, 3): the folder's documented decision
 *                             makes integer `/` the same operation as DIV */
static int64_t v_7_slash_3(void) { return (int64_t)rt_div(7, 3); }

/* (0 - 7) / 3            ==>  rt_div(0 - 7, 3): the row that discriminates the
 *                             integer-`/` decision from C's truncating `/` */
static int64_t v_m7_p_slash_3(void) { return (int64_t)rt_div(0 - 7, 3); }

/* LSL(1, 31)             ==>  rt_lsl(1, 31) */
static int64_t v_lsl_1_31(void) { return (int64_t)rt_lsl(1, 31); }

/* LSL(1, 32)             ==>  rt_lsl(1, 32)  [x * 2^32 wraps to 0] */
static int64_t v_lsl_1_32(void) { return (int64_t)rt_lsl(1, 32); }

/* LSL(1, 33)             ==>  rt_lsl(1, 33) */
static int64_t v_lsl_1_33(void) { return (int64_t)rt_lsl(1, 33); }

/* ASR(-8, 1)             ==>  rt_asr(0 - 8, 1) */
static int64_t v_asr_m8_1(void) { return (int64_t)rt_asr(0 - 8, 1); }

/* ASR(8, 1)              ==>  rt_asr(8, 1) */
static int64_t v_asr_8_1(void) { return (int64_t)rt_asr(8, 1); }

/* ASR(-1, 31)            ==>  rt_asr(0 - 1, 31) */
static int64_t v_asr_m1_31(void) { return (int64_t)rt_asr(0 - 1, 31); }

/* ROR(1, 1)              ==>  (int32_t)rt_ror(1u, 1): ROR yields an INTEGER,
 *                             so the rotated pattern is read back as int32_t */
static int64_t v_ror_1_1(void) { return (int64_t)(int32_t)rt_ror(1u, 1); }

/* ROR(5, 0)              ==>  (int32_t)rt_ror(5u, 0) */
static int64_t v_ror_5_0(void) { return (int64_t)(int32_t)rt_ror(5u, 0); }

/* FLOOR(-1.5)            ==>  rt_floor(-1.5f) */
static int64_t v_floor_m1_5(void) { return (int64_t)rt_floor(-1.5f); }

/* FLOOR(-0.5)            ==>  rt_floor(-0.5f) */
static int64_t v_floor_m0_5(void) { return (int64_t)rt_floor(-0.5f); }

/* FLOOR(1.5)             ==>  rt_floor(1.5f) */
static int64_t v_floor_1_5(void) { return (int64_t)rt_floor(1.5f); }

/* FLT(7)                 ==>  (int64_t)rt_flt(7) */
static int64_t v_flt_7(void) { return (int64_t)rt_flt(7); }

/* ORD("A")               ==>  rt_ord("A"[0])   [a 1-character string is a CHAR] */
static int64_t v_ord_A(void) { return (int64_t)rt_ord("A"[0]); }

/* CHR(65)                ==>  rt_ord((char)65): the value of the CHAR is its ordinal */
static int64_t v_chr_65(void) { return (int64_t)rt_ord((char)65); }

/* CHR(ORD("A"))          ==>  rt_ord((char)rt_ord("A"[0])) */
static int64_t v_chr_ord_A(void) { return (int64_t)rt_ord((char)rt_ord("A"[0])); }

/* ABS(-7)                ==>  the report's definition, x < 0 ? -x : x */
static int64_t v_abs_m7(void) {
  int32_t x = 0 - 7;
  return (int64_t)(x < 0 ? -x : x);
}

/* ABS(0)                 ==>  x < 0 ? -x : x */
static int64_t v_abs_0(void) {
  int32_t x = 0;
  return (int64_t)(x < 0 ? -x : x);
}

/* ODD(-3)                ==>  rt_mod(-3, 2) != 0 */
static int64_t v_odd_m3(void) { return (int64_t)(rt_mod(0 - 3, 2) != 0); }

/* ODD(0)                 ==>  rt_mod(0, 2) != 0 */
static int64_t v_odd_0(void) { return (int64_t)(rt_mod(0, 2) != 0); }

/* ~TRUE                  ==>  (!1) */
static int64_t v_not_true(void) { return (int64_t)(!1); }

/* TRUE OR FALSE          ==>  (1 || 0) */
static int64_t v_true_or_false(void) { return (int64_t)(1 || 0); }

/* (0 <= 3) & (3 < 100)   ==>  ((0 <= 3) && (3 < 100)) */
static int64_t v_and_le_lt(void) { return (int64_t)((0 <= 3) && (3 < 100)); }

/* 3 IN {1 .. 5}          ==>  rt_in(rt_set(1, 5), 3) */
static int64_t v_3_in_1_5(void) { return (int64_t)rt_in(rt_set(1, 5), 3); }

/* 9 IN {1 .. 5}          ==>  rt_in(rt_set(1, 5), 9) */
static int64_t v_9_in_1_5(void) { return (int64_t)rt_in(rt_set(1, 5), 9); }

/* 0 IN {}                ==>  rt_in(0u, 0): the empty set literal has no range */
static int64_t v_0_in_empty(void) { return (int64_t)rt_in(0u, 0); }

/* 31 IN {0 .. 31}        ==>  rt_in(rt_set(0, 31), 31) */
static int64_t v_31_in_0_31(void) { return (int64_t)rt_in(rt_set(0, 31), 31); }

/* {1 .. 3}               ==>  rt_set(1, 3); a SET prints as its unsigned pattern */
static int64_t v_set_1_3(void) { return (int64_t)(uint32_t)rt_set(1, 3); }

/* {5 .. 1}               ==>  rt_set(5, 1)   [empty] */
static int64_t v_set_5_1(void) { return (int64_t)(uint32_t)rt_set(5, 1); }

/* {1, 3 .. 4}            ==>  rt_set(1, 1) | rt_set(3, 4) */
static int64_t v_set_1_3_4(void) { return (int64_t)(uint32_t)(rt_set(1, 1) | rt_set(3, 4)); }

/* {0 .. 31}              ==>  rt_set(0, 31) */
static int64_t v_set_0_31(void) { return (int64_t)(uint32_t)rt_set(0, 31); }

/* {1 .. 3} + {0}         ==>  rt_set(1, 3) | rt_set(0, 0) */
static int64_t v_set_union(void) { return (int64_t)(uint32_t)(rt_set(1, 3) | rt_set(0, 0)); }

/* {1 .. 3} - {2}         ==>  rt_set(1, 3) & ~rt_set(2, 2) */
static int64_t v_set_diff(void) { return (int64_t)(uint32_t)(rt_set(1, 3) & ~rt_set(2, 2)); }

/* {1 .. 3} * {2 .. 4}    ==>  rt_set(1, 3) & rt_set(2, 4) */
static int64_t v_set_inter(void) { return (int64_t)(uint32_t)(rt_set(1, 3) & rt_set(2, 4)); }

/* {1 .. 2} / {2 .. 3}    ==>  rt_set(1, 2) ^ rt_set(2, 3) */
static int64_t v_set_symdiff(void) { return (int64_t)(uint32_t)(rt_set(1, 2) ^ rt_set(2, 3)); }

/* -{0}                   ==>  ~rt_set(0, 0): unary `-` is the set complement */
static int64_t v_neg_set_0(void) { return (int64_t)(uint32_t)(~rt_set(0, 0)); }

/* "abc" < "abd"          ==>  rt_charcmp("abc", 4, "abd", 4) < 0 (DESIGN 9.5) */
static int64_t v_str_lt(void) { return (int64_t)(rt_charcmp("abc", 4, "abd", 4) < 0); }

typedef struct {
  const char *expr;
  int64_t (*value)(void);
} ConstExprRow;

static const ConstExprRow const_expr_rows[] = {
    {"2*100 - 1", v_2_times_100_minus_1},
    {"100H", v_100H},
    {"0FFFFFFFFH", v_0FFFFFFFFH},
    {"7FFFFFFFH", v_7FFFFFFFH},
    {"7 DIV 3", v_7_div_3},
    {"7 MOD 3", v_7_mod_3},
    {"-7 DIV 3", v_m7_div_3},
    {"-7 MOD 3", v_m7_mod_3},
    {"7 DIV (0 - 3)", v_7_div_m3},
    {"7 MOD (0 - 3)", v_7_mod_m3},
    {"(0 - 7) DIV 3", v_m7_p_div_3},
    {"(0 - 7) MOD 3", v_m7_p_mod_3},
    {"(0 - 7) DIV (0 - 3)", v_m7_p_div_m3},
    {"(0 - 7) MOD (0 - 3)", v_m7_p_mod_m3},
    {"7 / 3", v_7_slash_3},
    {"(0 - 7) / 3", v_m7_p_slash_3},
    {"LSL(1, 31)", v_lsl_1_31},
    {"LSL(1, 32)", v_lsl_1_32},
    {"LSL(1, 33)", v_lsl_1_33},
    {"ASR(-8, 1)", v_asr_m8_1},
    {"ASR(8, 1)", v_asr_8_1},
    {"ASR(-1, 31)", v_asr_m1_31},
    {"ROR(1, 1)", v_ror_1_1},
    {"ROR(5, 0)", v_ror_5_0},
    {"FLOOR(-1.5)", v_floor_m1_5},
    {"FLOOR(-0.5)", v_floor_m0_5},
    {"FLOOR(1.5)", v_floor_1_5},
    {"FLT(7)", v_flt_7},
    {"ORD(\"A\")", v_ord_A},
    {"CHR(65)", v_chr_65},
    {"CHR(ORD(\"A\"))", v_chr_ord_A},
    {"ABS(-7)", v_abs_m7},
    {"ABS(0)", v_abs_0},
    {"ODD(-3)", v_odd_m3},
    {"ODD(0)", v_odd_0},
    {"~TRUE", v_not_true},
    {"TRUE OR FALSE", v_true_or_false},
    {"(0 <= 3) & (3 < 100)", v_and_le_lt},
    {"3 IN {1 .. 5}", v_3_in_1_5},
    {"9 IN {1 .. 5}", v_9_in_1_5},
    {"0 IN {}", v_0_in_empty},
    {"31 IN {0 .. 31}", v_31_in_0_31},
    {"{1 .. 3}", v_set_1_3},
    {"{5 .. 1}", v_set_5_1},
    {"{1, 3 .. 4}", v_set_1_3_4},
    {"{0 .. 31}", v_set_0_31},
    {"{1 .. 3} + {0}", v_set_union},
    {"{1 .. 3} - {2}", v_set_diff},
    {"{1 .. 3} * {2 .. 4}", v_set_inter},
    {"{1 .. 2} / {2 .. 3}", v_set_symdiff},
    {"-{0}", v_neg_set_0},
    {"\"abc\" < \"abd\"", v_str_lt}};

#define CONST_EXPR_ROWS ((int32_t)(sizeof const_expr_rows / sizeof const_expr_rows[0]))

int main(void) {
  FILE *f = fopen("constexpr.tsv", "w");
  int32_t i;
  if (f == NULL) {
    perror("constexpr.tsv");
    return 1;
  }
  fprintf(f,
          "# constexpr.tsv -- C values of the Oberon-07 constant expressions the\n"
          "# front end's folder folds (oberon/src/const_eval.rs).  Written by\n"
          "# constexpr.c, which computes every value with the runtime helpers the\n"
          "# back end emits (rt_div, rt_mod, rt_lsl, rt_asr, rt_ror, rt_set, rt_in,\n"
          "# rt_floor, rt_flt, rt_ord) -- no literal values, no other algorithm.\n"
          "# Regenerate with `bash oberon/golden/run.sh`.\n"
          "#\n"
          "# Format: one data row per expression, `<expression><TAB><decimal value>`.\n"
          "# Values: INTEGER/BOOLEAN/CHAR are signed decimal (two's complement for\n"
          "# 32-bit patterns); SET is the 32-bit pattern as an unsigned decimal;\n"
          "# REAL is truncated toward zero.\n"
          "#\n"
          "# Spelling notes:\n"
          "#  * `-7 DIV 3` is `-(7 DIV 3)`, the report's grammar binds a leading\n"
          "#    sign to the whole term (DESIGN 8.3), so it is NOT (-7) DIV 3; the\n"
          "#    parenthesised rows carry the negative-dividend cases.\n"
          "#  * `7 DIV -3` / `7 MOD -3` are not grammatical (no sign on a factor),\n"
          "#    so the negative divisor is spelled `(0 - 3)`.\n"
          "#  * `~{0}` is absent: unary `~` needs a BOOLEAN operand (report 8.2.1)\n"
          "#    and the folder rejects it; a SET complement is written `-{0}`.\n"
          "#  * integer `/` is the folder's documented DIV (see const_eval.rs);\n"
          "#    `(0 - 7) / 3` is the row that discriminates it from C's truncated `/`.\n");
  for (i = 0; i < CONST_EXPR_ROWS; i = i + 1) {
    if (fprintf(f, "%s\t%lld\n", const_expr_rows[i].expr,
                (long long)const_expr_rows[i].value()) < 0) {
      fclose(f);
      return 1;
    }
  }
  if (fclose(f) != 0) return 1;
  printf("constexpr.tsv: %d data rows written\n", (int)CONST_EXPR_ROWS);
  return 0;
}
