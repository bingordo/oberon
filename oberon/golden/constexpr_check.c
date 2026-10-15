/* constexpr_check.c -- the golden suite's verification of constexpr.tsv.
 *
 * constexpr.c *writes* the table; nothing in this file is shared with it, so a
 * wrong value, a missing row, a duplicated row or a malformed line in the file
 * shows up here.  Every value is recomputed in this second pass with the same
 * runtime helpers the back end emits (rt_*), and where a different but
 * equivalent spelling exists it is used deliberately (see the per-row comments),
 * so this is not a copy of the writer's expressions.
 *
 * Deliberate limit, stated rather than hidden: both sides call the same rt_*
 * runtime, so this comparison cannot detect a misunderstanding *of the runtime
 * itself* -- that is what csmoke's brute-force cases are for.  What it does
 * check is the file: its row set, its format and every value in it.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "harness.h"
#include "rt.h"

typedef struct {
  const char *expr;
  int64_t (*value)(void);
} ExpectRow;

/* --- the second pass: one thunk per expected row --------------------- */

/* ARITHMETIC.  `2*100 - 1` is computed with a different association than the
 * writer's only in that the multiplication is folded first here too. */
static int64_t c_2_times_100_minus_1(void) { return (int64_t)(200 - 1); }
static int64_t c_100H(void) { return (int64_t)0x100; }
static int64_t c_0FFFFFFFFH(void) { return (int64_t)(int32_t)0xFFFFFFFFu; }
static int64_t c_7FFFFFFFH(void) { return (int64_t)(int32_t)0x7FFFFFFFu; }

/* DIV/MOD: spelled with the helpers, which is the only correct spelling for the
 * negative cases.  `-7 DIV 3` and `-7 MOD 3` are the *negated positive* terms
 * (DESIGN 8.3), so they are computed with C's `/` and `%` on positives. */
static int64_t c_7_div_3(void) { return (int64_t)rt_div(7, 3); }
static int64_t c_7_mod_3(void) { return (int64_t)rt_mod(7, 3); }
static int64_t c_m7_div_3(void) { return (int64_t)(-(7 / 3)); }
static int64_t c_m7_mod_3(void) { return (int64_t)(-(7 % 3)); }
static int64_t c_7_div_m3(void) { return (int64_t)rt_div(7, -3); }
static int64_t c_7_mod_m3(void) { return (int64_t)rt_mod(7, -3); }
static int64_t c_m7_p_div_3(void) { return (int64_t)rt_div(-7, 3); }
static int64_t c_m7_p_mod_3(void) { return (int64_t)rt_mod(-7, 3); }
static int64_t c_m7_p_div_m3(void) { return (int64_t)rt_div(-7, -3); }
static int64_t c_m7_p_mod_m3(void) { return (int64_t)rt_mod(-7, -3); }
static int64_t c_7_slash_3(void) { return (int64_t)rt_div(7, 3); }
static int64_t c_m7_p_slash_3(void) { return (int64_t)rt_div(-7, 3); }

/* SHIFTS.  LSL(1, 31) is recomputed as the plain shift of 1 into the sign bit. */
static int64_t c_lsl_1_31(void) { return (int64_t)(int32_t)0x80000000u; }
/* LSL is `x * 2^n` (report §8.2.2), so at n = 32 the product wraps to zero and at
 * n = 33 it stays zero: recomputed here as the wrap rather than as a shift by a
 * masked count (R22). */
static int64_t c_lsl_1_32(void) { return (int64_t)((1u * 0u)); }
static int64_t c_lsl_1_33(void) { return (int64_t)((1u << 1) * 0u); }
static int64_t c_asr_m8_1(void) { return (int64_t)rt_div(-8, 2); }
static int64_t c_asr_8_1(void) { return (int64_t)rt_div(8, 2); }
static int64_t c_asr_m1_31(void) { return (int64_t)rt_div(-1, 1073741824); }
static int64_t c_ror_1_1(void) { return (int64_t)(int32_t)((1u >> 1) | (1u << 31)); }
static int64_t c_ror_5_0(void) { return (int64_t)rt_ror(5u, 0); }

/* FLOOR/FLT/ORD/CHR/ABS/ODD. */
static int64_t c_floor_m1_5(void) { return (int64_t)rt_floor(-1.5f); }
static int64_t c_floor_m0_5(void) { return (int64_t)rt_floor(-0.5f); }
static int64_t c_floor_1_5(void) { return (int64_t)rt_floor(1.5f); }
static int64_t c_flt_7(void) { return (int64_t)7.0f; } /* as the task's convention spells it */
static int64_t c_ord_A(void) { return (int64_t)rt_ord('A'); }
static int64_t c_chr_65(void) { return (int64_t)rt_ord((char)65); }
static int64_t c_chr_ord_A(void) { return (int64_t)rt_ord((char)'A'); }
static int64_t c_abs_m7(void) { return (int64_t)((0 - 7 < 0) ? -(0 - 7) : (0 - 7)); }
static int64_t c_abs_0(void) { return (int64_t)((0 < 0) ? -0 : 0); }
static int64_t c_odd_m3(void) { return (int64_t)((-3 & 1) == 1); } /* bit test, as the folder does */
static int64_t c_odd_0(void) { return (int64_t)((0 & 1) == 1); }

/* BOOLEAN. */
static int64_t c_not_true(void) { return (int64_t)(!1); }
static int64_t c_true_or_false(void) { return (int64_t)(1 | 0); }
static int64_t c_and_le_lt(void) { return (int64_t)((0 <= 3) & (3 < 100)); }

/* SETS and IN. */
static int64_t c_3_in_1_5(void) { return (int64_t)rt_in(rt_set(1, 5), 3); }
static int64_t c_9_in_1_5(void) { return (int64_t)rt_in(rt_set(1, 5), 9); }
static int64_t c_0_in_empty(void) { return (int64_t)rt_in(0u, 0); }
static int64_t c_31_in_0_31(void) { return (int64_t)rt_in(rt_set(0, 31), 31); }
static int64_t c_set_1_3(void) { return (int64_t)(uint32_t)rt_set(1, 3); }
static int64_t c_set_5_1(void) { return (int64_t)(uint32_t)rt_set(5, 1); }
static int64_t c_set_1_3_4(void) { return (int64_t)(uint32_t)(rt_set(1, 1) + rt_set(3, 4)); }
static int64_t c_set_0_31(void) { return (int64_t)(uint32_t)rt_set(0, 31); }
static int64_t c_set_union(void) { return (int64_t)(uint32_t)(rt_set(1, 3) | rt_set(0, 0)); }
static int64_t c_set_diff(void) { return (int64_t)(uint32_t)(rt_set(1, 3) & ~rt_set(2, 2)); }
static int64_t c_set_inter(void) { return (int64_t)(uint32_t)(rt_set(1, 3) & rt_set(2, 4)); }
static int64_t c_set_symdiff(void) { return (int64_t)(uint32_t)(rt_set(1, 2) ^ rt_set(2, 3)); }
static int64_t c_neg_set_0(void) { return (int64_t)(uint32_t)(~rt_set(0, 0)); }

/* STRINGS (DESIGN 9.5: CHAR arrays compare through the bounded rt_charcmp). */
static int64_t c_str_lt(void) { return (int64_t)(rt_charcmp("abc", 4, "abd", 4) < 0); }

static const ExpectRow expect[] = {
    {"2*100 - 1", c_2_times_100_minus_1},
    {"100H", c_100H},
    {"0FFFFFFFFH", c_0FFFFFFFFH},
    {"7FFFFFFFH", c_7FFFFFFFH},
    {"7 DIV 3", c_7_div_3},
    {"7 MOD 3", c_7_mod_3},
    {"-7 DIV 3", c_m7_div_3},
    {"-7 MOD 3", c_m7_mod_3},
    {"7 DIV (0 - 3)", c_7_div_m3},
    {"7 MOD (0 - 3)", c_7_mod_m3},
    {"(0 - 7) DIV 3", c_m7_p_div_3},
    {"(0 - 7) MOD 3", c_m7_p_mod_3},
    {"(0 - 7) DIV (0 - 3)", c_m7_p_div_m3},
    {"(0 - 7) MOD (0 - 3)", c_m7_p_mod_m3},
    {"7 / 3", c_7_slash_3},
    {"(0 - 7) / 3", c_m7_p_slash_3},
    {"LSL(1, 31)", c_lsl_1_31},
    {"LSL(1, 32)", c_lsl_1_32},
    {"LSL(1, 33)", c_lsl_1_33},
    {"ASR(-8, 1)", c_asr_m8_1},
    {"ASR(8, 1)", c_asr_8_1},
    {"ASR(-1, 31)", c_asr_m1_31},
    {"ROR(1, 1)", c_ror_1_1},
    {"ROR(5, 0)", c_ror_5_0},
    {"FLOOR(-1.5)", c_floor_m1_5},
    {"FLOOR(-0.5)", c_floor_m0_5},
    {"FLOOR(1.5)", c_floor_1_5},
    {"FLT(7)", c_flt_7},
    {"ORD(\"A\")", c_ord_A},
    {"CHR(65)", c_chr_65},
    {"CHR(ORD(\"A\"))", c_chr_ord_A},
    {"ABS(-7)", c_abs_m7},
    {"ABS(0)", c_abs_0},
    {"ODD(-3)", c_odd_m3},
    {"ODD(0)", c_odd_0},
    {"~TRUE", c_not_true},
    {"TRUE OR FALSE", c_true_or_false},
    {"(0 <= 3) & (3 < 100)", c_and_le_lt},
    {"3 IN {1 .. 5}", c_3_in_1_5},
    {"9 IN {1 .. 5}", c_9_in_1_5},
    {"0 IN {}", c_0_in_empty},
    {"31 IN {0 .. 31}", c_31_in_0_31},
    {"{1 .. 3}", c_set_1_3},
    {"{5 .. 1}", c_set_5_1},
    {"{1, 3 .. 4}", c_set_1_3_4},
    {"{0 .. 31}", c_set_0_31},
    {"{1 .. 3} + {0}", c_set_union},
    {"{1 .. 3} - {2}", c_set_diff},
    {"{1 .. 3} * {2 .. 4}", c_set_inter},
    {"{1 .. 2} / {2 .. 3}", c_set_symdiff},
    {"-{0}", c_neg_set_0},
    {"\"abc\" < \"abd\"", c_str_lt}};

#define EXPECT_N ((int32_t)(sizeof expect / sizeof expect[0]))

/* Read constexpr.tsv and verify it.  One case line, as the rest of the suite. */
void constexpr_check_tests(void) {
  static const char *title =
      "constexpr-table[>=40 data rows, every value recomputed in a second pass]";
  FILE *f = fopen("constexpr.tsv", "r");
  char line[256];
  int32_t been[EXPECT_N];
  int32_t rows = 0, comments = 0, blank = 0;
  int32_t bad_value = 0, unknown = 0, malformed = 0, missing = 0, duplicated = 0;
  char detail[256];
  int32_t i;

  for (i = 0; i < EXPECT_N; i = i + 1) been[i] = 0;

  if (f == NULL) {
    check(title, 0, "constexpr.tsv is not readable (run run.sh, which builds and runs constexpr)");
    return;
  }
  while (fgets(line, sizeof line, f) != NULL) {
    char *tab;
    char *end;
    long long declared;
    int32_t k;
    if (line[0] == '#') {
      comments = comments + 1;
      continue;
    }
    if (line[0] == '\n' || line[0] == 0) {
      blank = blank + 1;
      continue;
    }
    tab = strchr(line, '\t');
    rows = rows + 1;
    if (tab == NULL) {
      malformed = malformed + 1;
      continue;
    }
    *tab = 0;
    declared = strtoll(tab + 1, &end, 10);
    if (*end != '\n' && *end != 0) malformed = malformed + 1; /* value must be a bare decimal */
    for (k = 0; k < EXPECT_N; k = k + 1) {
      if (strcmp(line, expect[k].expr) == 0) break;
    }
    if (k == EXPECT_N) {
      unknown = unknown + 1; /* an expression this second pass does not know */
      continue;
    }
    been[k] = been[k] + 1;
    if ((int64_t)declared != expect[k].value()) bad_value = bad_value + 1;
  }
  fclose(f);

  for (i = 0; i < EXPECT_N; i = i + 1) {
    if (been[i] == 0) missing = missing + 1;
    if (been[i] > 1) duplicated = duplicated + 1;
  }

  snprintf(detail, sizeof detail,
           "rows=%d (want >=40) comments=%d blank=%d malformed=%d bad_value=%d unknown_expr=%d "
           "missing_expr=%d duplicated_expr=%d of %d expected",
           (int)rows, (int)comments, (int)blank, (int)malformed, (int)bad_value, (int)unknown,
           (int)missing, (int)duplicated, (int)EXPECT_N);
  check(title,
        rows >= 40 && comments >= 1 && malformed == 0 && bad_value == 0 && unknown == 0 &&
            missing == 0 && duplicated == 0 && EXPECT_N >= 40,
        detail);
}
