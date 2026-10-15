/* smoke.c -- evidence for Oberon-07 -> C code-generation rules.
 *
 * Build: gcc -std=c11 -O2 -fwrapv -Wall -Wextra -Werror -o smoke smoke.c
 * Prints one "PASS <case>" or "FAIL <case>: <detail>" line per case.
 * Exits non-zero if any assertion failed.
 *
 * Style: straight-line, as a code generator would emit it. The block below
 * is the runtime helper section for the Oberon predeclared operations used
 * by the cases.
 */
#include <stdint.h>
#include <stdio.h>

/* ==================================================================== */
/* runtime helpers (emitted by the backend for predeclared operations)   */
/* ==================================================================== */

/* DIV / MOD : floor division. The backend defines
 *     q = x DIV y = floor(x / y)
 *     r = x MOD y = x - q*y        (so 0 <= r < y for y > 0)
 * Emitted code:   q = x/y; r = x%y; if (r!=0 && (r<0)!=(y<0)) { q--; r+=y; }
 * y == -1 is special-cased so the x86 idiv INT32_MIN/-1 #DE trap cannot fire;
 * the special case reproduces the two's-complement (-fwrapv) wrap result.  */
static int32_t rt_div(int32_t x, int32_t y) {
  if (y == -1) return (int32_t)(0u - (uint32_t)x); /* -x with 2's-complement wrap */
  int32_t q = x / y;
  int32_t r = x % y;
  if (r != 0 && (r < 0) != (y < 0)) q = q - 1;
  return q;
}
static int32_t rt_mod(int32_t x, int32_t y) {
  if (y == -1) return 0;
  int32_t r = x % y;
  if (r != 0 && (r < 0) != (y < 0)) r = r + y;
  return r;
}

/* SET is uint32_t; elements 0..31.
 * {a .. b}  ==>  ((1u << (b-a+1)) - 1u) << a     (empty when a > b)
 * with a clamped to 0, b clamped to 31, and a width-32 case handled as
 * 0xFFFFFFFFu << a.  This is the exact C expression the backend emits.   */
static uint32_t rt_set(int32_t a, int32_t b) {
  if (a < 0) a = 0;
  if (b > 31) b = 31;
  if (a > b) return 0u;
  if (b == 31) return 0xFFFFFFFFu << a;
  return ((1u << (b - a + 1)) - 1u) << a;
}
/* x IN s  ==>  (s >> (x & 31)) & 1u  */
static int32_t rt_in(uint32_t s, int32_t e) {
  return (int32_t)((s >> (e & 31)) & 1u);
}

/* FOR v := beg TO end BY inc DO S END
 * emitted:  for (v = beg; inc > 0 ? v <= end : v >= end; v += inc) S       */
static int32_t rt_for_count(int32_t beg, int32_t end, int32_t inc) {
  int32_t n = 0;
  int32_t v;
  for (v = beg; inc > 0 ? v <= end : v >= end; v += inc) n = n + 1;
  return n;
}

/* CHAR arrays (ARRAY OF CHAR) are NUL-terminated char arrays.
 * Assignment zero-fills the unused tail (backend choice).                  */
static void rt_strcopy(char *dst, int32_t dstlen, const char *src) {
  int32_t i = 0;
  while (i < dstlen && src[i] != 0) { dst[i] = src[i]; i = i + 1; }
  while (i < dstlen) { dst[i] = 0; i = i + 1; }
}
/* Lexicographic comparison like strcmp; relational operators wrap this.    */
static int32_t rt_strcmp(const char *a, const char *b) {
  int32_t i = 0;
  while (a[i] != 0 && a[i] == b[i]) i = i + 1;
  return (int32_t)(unsigned char)a[i] - (int32_t)(unsigned char)b[i];
}
static int32_t rt_strlt(const char *a, const char *b) { return rt_strcmp(a, b) < 0; }
static int32_t rt_streq(const char *a, const char *b) { return rt_strcmp(a, b) == 0; }

/* Type descriptors for extensible records.
 * A pointer to a record targets a struct whose FIRST word is a pointer to a
 * static type descriptor; each descriptor holds a pointer to its base
 * descriptor (NULL for a root type).                                       */
typedef struct Desc Desc;
/* The backend extends the descriptor with a unique integer id used as the
 * discriminator of a type CASE:  switch (RT_DESC(v)->id) { case TD_R0: ... } */
struct Desc { const Desc *base; const char *name; int32_t id; };

typedef struct R  R;
typedef struct R0 R0;
typedef struct R1 R1;
typedef struct R2 R2;
/* Record types: extension struct's FIRST member is the base struct, and a
 * record struct's FIRST word is its descriptor pointer.                    */
struct R  { const Desc *desc; int32_t x; };
struct R0 { R base; int32_t y; }; /* R0 extends R */
struct R1 { R base; int32_t z; }; /* R1 extends R */
struct R2 { R base; int32_t w; }; /* R2 extends R */

/* descriptor ids (TD_*), unique per type */
enum { TD_R = 0, TD_R0 = 1, TD_R1 = 2, TD_R2 = 3 };

static const Desc D_R  = { NULL, "R",  TD_R  };
static const Desc D_R0 = { &D_R, "R0", TD_R0 };
static const Desc D_R1 = { &D_R, "R1", TD_R1 };
static const Desc D_R2 = { &D_R, "R2", TD_R2 };

static const Desc *rt_dyn(const void *p) { return *(const Desc *const *)p; }
static int32_t rt_is(const Desc *d, const Desc *target) {
  while (d != NULL) {
    if (d == target) return 1;
    d = d->base;
  }
  return 0;
}

/* IS guard: a failed type test traps (recorded here rather than aborting). */
static int32_t rt_traps = 0;
static void rt_trap(const char *msg) { (void)msg; rt_traps = rt_traps + 1; }
static void rt_guard(int32_t cond, const char *msg) { if (!cond) rt_trap(msg); }

/* Shifts.  report: LSL(x,n) = x * 2^n ; ASR(x,n) = x DIV 2^n ; ROR = rotate
 * right by n.  n is reduced modulo 32.                                     */
static int32_t rt_lsl(int32_t x, int32_t n) { return (int32_t)((uint32_t)x << (n & 31)); }
static int32_t rt_asr(int32_t x, int32_t n) { return x >> (n & 31); } /* gcc: arithmetic */
static uint32_t rt_ror(uint32_t x, int32_t n) {
  n = n & 31;
  if (n == 0) return x;
  return (x >> n) | (x << (32 - n));
}

/* Open arrays: callee receives a pointer plus a hidden length parameter.    */
static int32_t rt_oa_len_seen;
static void rt_oa_probe(const int32_t *a, int32_t a__len) {
  (void)a;
  rt_oa_len_seen = a__len; /* LEN(a) inside the callee */
}

/* Parameter modes. */
static void rt_pval(R *v) { v->x = 999; }         /* value param: caller's copy */
static void rt_pvar(int32_t *v) { *v = *v + 100; } /* VAR param: caller's variable */

/* FLOOR(x: REAL): INTEGER truncates toward -inf; FLT(x): REAL identity;
 * ORD(ch) gives the ordinal.  (No libm: manual floor to keep the link line
 * exactly the one specified.)                                              */
static int32_t rt_floor(float x) {
  int32_t t = (int32_t)x;
  if ((float)t > x) t = t - 1;
  return t;
}
static float rt_flt(int32_t x) { return (float)x; }
static int32_t rt_ord(char c) { return (int32_t)(unsigned char)c; }

/* ==================================================================== */
/* R1: nested procedures and static links                               */
/* ==================================================================== */

/* outer -> middle -> inner.  outer owns x.  middle receives the hidden
 * static link (void *sl) to outer's frame and caches it as a typed frame
 * pointer.  inner receives a static link to middle's frame and reaches
 * outer's x through middle's saved frame pointer.  r1_plain references
 * nothing from an enclosing frame, so it gets NO link parameter.          */
typedef struct { int32_t x; } F_outer;
typedef struct { void *sl; F_outer *outer; } F_middle;

static int32_t r1_plain_calls = 0;
static int32_t r1_outer_result = -1;

/* no enclosing-frame references ==> plain void(void) signature */
static void r1_plain(void) { r1_plain_calls = r1_plain_calls + 1; }

static void r1_inner(void *sl);

/* takes the static link: void(void *) */
static void r1_middle(void *sl) {
  F_middle f;
  f.sl = sl;
  f.outer = (F_outer *)f.sl;        /* link used to reach outer */
  f.outer->x = f.outer->x + 1;      /* middle mutates sl_outer->x */
  r1_inner((void *)&f);             /* inner's link -> middle's frame */
}

static void r1_inner(void *sl) {
  F_middle *fm = (F_middle *)sl;
  fm->outer->x = fm->outer->x + 40; /* outer.x via middle's saved frame ptr */
}

/* emitted body of outer: declares its frame, passes its address as the link */
static void r1_outer(void) {
  F_outer fo;
  fo.x = 0;
  r1_middle((void *)&fo);
  r1_outer_result = fo.x;
}

/* ==================================================================== */
/* R2: type CASE on records/pointers via descriptor id                  */
/* ==================================================================== */

/* emitted form:  RT_DESC(v)->id  -- first word of the heap block is the
 * descriptor pointer.  Arms narrow-cast v; default traps.                 */
#define RT_DESC(v) (*(const Desc *const *)(v))

static int32_t r2_case_ran = 0;
static int32_t r2_r1_field = -1;

static void r2_typecase(void *v) {
  switch (RT_DESC(v)->id) {
    case TD_R0: {
      R0 *r0 = (R0 *)v;
      r2_case_ran = 1;
      (void)r0;
      break;
    }
    case TD_R1: {
      R1 *r1 = (R1 *)v;
      r2_case_ran = 2;
      r2_r1_field = r1->z;          /* arm-local narrowing cast */
      break;
    }
    case TD_R2: {
      R2 *r2 = (R2 *)v;
      r2_case_ran = 3;
      (void)r2;
      break;
    }
    default:
      rt_trap("type CASE: no matching arm");
      break;
  }
}

/* ==================================================================== */
/* R3: structured value parameter of array type (caller copies)         */
/* ==================================================================== */
#define R3_N 3
typedef struct { int32_t e[R3_N]; } R3Arr;

static int32_t r3_seen = -1;
/* value parameter of array type: callee gets a pointer to the caller's copy */
static void r3_callee(R3Arr *p) {
  r3_seen = p->e[0];                /* callee sees the original contents */
  p->e[0] = 99;                     /* mutate the (private) copy */
  p->e[1] = p->e[1] + 1;
}

/* ==================================================================== */
/* tiny test harness                                                    */
/* ==================================================================== */
static int32_t failures = 0;
static void check(const char *name, int32_t cond, const char *detail) {
  if (cond) {
    printf("PASS %s\n", name);
  } else {
    printf("FAIL %s: %s\n", name, detail);
    failures = failures + 1;
  }
}

/* ==================================================================== */
/* case 1: DIV / MOD                                                     */
/* ==================================================================== */
static void case_divmod(void) {
  int32_t bad = 0;
  char detail[128];
  detail[0] = 0;
  for (int32_t x = -64; x <= 64; x = x + 1) {
    for (int32_t y = -64; y <= 64; y = y + 1) {
      if (y == 0) continue;
      int32_t q = rt_div(x, y);
      int32_t r = rt_mod(x, y);
      int32_t ay = y > 0 ? y : -y;
      int32_t ar = r > 0 ? r : -r;
      if (x != q * y + r) {
        bad = bad + 1;
        snprintf(detail, sizeof detail, "x=%d y=%d q=%d r=%d: x != q*y+r", (int)x, (int)y, (int)q, (int)r);
      }
      if (!(ar < ay)) {
        bad = bad + 1;
        snprintf(detail, sizeof detail, "x=%d y=%d r=%d: |r| >= |y|", (int)x, (int)y, (int)r);
      }
      if (!(r == 0 || ((r > 0) == (y > 0)))) {
        bad = bad + 1;
        snprintf(detail, sizeof detail, "x=%d y=%d r=%d: sign(r) != sign(y)", (int)x, (int)y, (int)r);
      }
      if (y > 0 && !(r >= 0 && r < y)) {
        bad = bad + 1;
        snprintf(detail, sizeof detail, "x=%d y=%d r=%d: not 0<=r<y", (int)x, (int)y, (int)r);
      }
    }
  }
  check("divmod-brute-force[-64..64]", bad == 0, detail);

  check("divmod-by-1[7 DIV 1 = 7, 7 MOD 1 = 0]",
        rt_div(7, 1) == 7 && rt_mod(7, 1) == 0, "DIV/MOD by 1 wrong");
  check("divmod-by--1[-7 DIV -1 = 7, -7 MOD -1 = 0]",
        rt_div(-7, -1) == 7 && rt_mod(-7, -1) == 0, "DIV/MOD by -1 wrong");

  {
    int32_t q = rt_div(INT32_MIN, -1);
    int32_t r = rt_mod(INT32_MIN, -1);
    check("divmod-INT32MIN-by--1[observed: q=INT32_MIN, r=0; q*y+r wraps to x under -fwrapv]",
          q == INT32_MIN && r == 0 && (int32_t)((uint32_t)q * (uint32_t)-1 + (uint32_t)r) == INT32_MIN,
          "INT32_MIN DIV -1 unexpected (overflow is undefined by the report; observed under -fwrapv)");
  }
}

/* ==================================================================== */
/* case 2: SET literals and operators                                    */
/* ==================================================================== */
static void case_sets(void) {
  check("set-lit-{1..3}[expr ((1u<<(b-a+1))-1u)<<a => 0b1110]", rt_set(1, 3) == 0xEu, "expected 0xE");
  check("set-lit-{1..1} => 0b10", rt_set(1, 1) == 0x2u, "expected 0x2");
  check("set-lit-{5..1} => empty", rt_set(5, 1) == 0u, "expected 0");
  check("set-lit-{0..31} => 0xFFFFFFFF", rt_set(0, 31) == 0xFFFFFFFFu, "expected 0xFFFFFFFF");
  check("set-lit-{0..0} => 1", rt_set(0, 0) == 1u, "expected 1");
  check("set-lit-{31..31} => 0x80000000", rt_set(31, 31) == 0x80000000u, "expected 0x80000000");

  check("set-IN-1-in-{1..1}", rt_in(rt_set(1, 1), 1) == 1, "1 IN {1..1} false");
  check("set-IN-0-in-{}", rt_in(0u, 0) == 0, "0 IN {} true");
  check("set-IN-31-in-{0..31}", rt_in(rt_set(0, 31), 31) == 1, "31 IN {0..31} false");

  {
    uint32_t a = rt_set(1, 3); /* 0b01110 = 14 */
    uint32_t b = rt_set(2, 4); /* 0b11100 = 28 */
    check("set-union+", (a | b) == 0x1Eu, "union wrong");
    check("set-difference-", (a & ~b) == 0x2u, "difference wrong");
    check("set-intersection*", (a & b) == 0xCu, "intersection wrong");
    check("set-symdiff/", (a ^ b) == 0x12u, "symmetric difference wrong");
    check("set-complement~", (~a) == 0xFFFFFFF1u, "complement wrong");
  }
}

/* ==================================================================== */
/* case 3: FOR loops                                                     */
/* ==================================================================== */
static void case_for(void) {
  check("for-(beg=7,end=3,inc=+1)->0", rt_for_count(7, 3, 1) == 0, "expected 0");
  check("for-(beg=1,end=5,inc=+2)->3", rt_for_count(1, 5, 2) == 3, "expected 3");
  check("for-(beg=5,end=1,inc=-2)->3", rt_for_count(5, 1, -2) == 3, "expected 3");
  check("for-(beg=1,end=5,inc=-1)->0", rt_for_count(1, 5, -1) == 0, "expected 0");
  check("for-(beg=1,end=1,inc=+1)->1", rt_for_count(1, 1, 1) == 1, "expected 1");
  check("for-(beg=5,end=5,inc=-1)->1", rt_for_count(5, 5, -1) == 1, "expected 1");
}

/* ==================================================================== */
/* case 4: CHAR array assignment and comparison                          */
/* ==================================================================== */
static void case_strings(void) {
  char a[5];
  rt_strcopy(a, 5, "A");
  check("char-array-assign-A", a[0] == 'A' && a[1] == 0 && a[2] == 0 && a[3] == 0 && a[4] == 0,
        "expected {'A',0,0,0,0}");
  check("char-array-cmp-'abc'<'abd'", rt_strlt("abc", "abd"), "expected true");
  check("char-array-cmp-'abc'=='abc'", rt_streq("abc", "abc"), "expected true");
  check("char-array-cmp-'ab'<'abc'", rt_strlt("ab", "abc"), "expected true");
  check("char-array-cmp-'abc'<'abc'==false", !rt_strlt("abc", "abc"), "expected false");
}

/* ==================================================================== */
/* case 5: type descriptors and IS                                       */
/* ==================================================================== */
static void case_types(void) {
  R0 heap;                 /* a heap block of dynamic type R0 */
  R0 *p = &heap;           /* P0 = POINTER TO R0 */
  heap.base.desc = &D_R0;  /* descriptor stored in the block's first word */

  check("IS-P0-is-R0", rt_is(rt_dyn(p), &D_R0) == 1, "p IS R0 false");
  check("IS-P0-is-R1-false", rt_is(rt_dyn(p), &D_R1) == 0, "p IS R1 true");
  check("IS-P0-is-R-base", rt_is(rt_dyn(p), &D_R) == 1, "p IS R false");

  {
    R rv;                       /* record variable of static type R */
    rv.desc = &D_R;             /* dynamic descriptor is the static type's */
    check("IS-var-static-desc", rt_is(rv.desc, &D_R) == 1 && rt_is(rv.desc, &D_R0) == 0,
          "record variable descriptor wrong");
  }
  rt_traps = 0;
  rt_guard(rt_is(rt_dyn(p), &D_R1), "R1 type guard"); /* false -> trap */
  check("IS-guard-traps-on-failure", rt_traps == 1, "guard on failed IS did not trap");
}

/* ==================================================================== */
/* case 6: shifts                                                       */
/* ==================================================================== */
static void case_shifts(void) {
  check("lsl(1,31)==INT32_MIN", rt_lsl(1, 31) == INT32_MIN, "expected INT32_MIN");
  check("lsl(1,32)==1[n mod 32]", rt_lsl(1, 32) == 1, "expected 1");
  check("asr(-8,1)==-4", rt_asr(-8, 1) == -4, "expected -4");
  check("asr(-1,1)==-1", rt_asr(-1, 1) == -1, "expected -1");
  check("ror(1,1)==0x80000000", rt_ror(1u, 1) == 0x80000000u, "expected 0x80000000");
  check("ror(5,0)==5", rt_ror(5u, 0) == 5u, "expected 5");

  {
    /* The task specification asserts ROR(0x80000000,1) == 1.  That value is
     * inconsistent with the Oberon-07 report (ROR = "x rotated right by n
     * bits", 10.2) and with the sibling assertion ROR(1,1) == 0x80000000:
     * a single right-rotation operation must send 0x80000000 to 0x40000000.
     * We keep the honest assertion (the true right-rotation result) and print
     * a FAIL-style line documenting the specification error. */
    uint32_t got = rt_ror(0x80000000u, 1);
    printf("FAIL ror(0x80000000,1): specification expects 1, but Oberon-07 ROR (rotate right) "
           "yields 0x%08X; value 1 is ROL(0x80000000,1). SKIPPED -- see note above.\n",
           (unsigned)got);
    check("ror(0x80000000,1)==0x40000000[honest right-rotation result]", got == 0x40000000u,
          "unexpected ROR result");
  }
}

/* ==================================================================== */
/* case 7: open arrays                                                   */
/* ==================================================================== */
static void case_open_arrays(void) {
  int32_t one[1] = { 5 };
  int32_t seven[7] = { 1, 2, 3, 4, 5, 6, 7 };

  rt_oa_probe(one, 0);
  check("open-array-LEN-0", rt_oa_len_seen == 0, "callee saw wrong length");
  rt_oa_probe(one, 1);
  check("open-array-LEN-1", rt_oa_len_seen == 1, "callee saw wrong length");
  rt_oa_probe(seven, 7);
  check("open-array-LEN-7", rt_oa_len_seen == 7, "callee saw wrong length");
}

/* ==================================================================== */
/* case 8: parameter modes                                               */
/* ==================================================================== */
static void case_params(void) {
  {
    R rv;
    rv.desc = &D_R;
    rv.x = 1;
    R copy = rv;      /* value parameter: caller copies the structured actual */
    rt_pval(&copy);   /* callee mutates its pointer target (read-only by contract) */
    check("value-param-record-not-visible", rv.x == 1, "caller variable was mutated");
    check("value-param-copy-mutated", copy.x == 999, "copy was not mutated (callee did not write)");
  }
  {
    int32_t n = 5;
    rt_pvar(&n);      /* VAR parameter: pointer to the caller's variable */
    check("var-param-integer-visible", n == 105, "VAR mutation not visible");
  }
}

/* ==================================================================== */
/* case 9: FLOOR / FLT / ORD                                             */
/* ==================================================================== */
static void case_math(void) {
  check("floor(-1.5)==-2", rt_floor(-1.5f) == -2, "expected -2");
  check("floor(1.5)==1", rt_floor(1.5f) == 1, "expected 1");
  check("floor(-0.5)==-1", rt_floor(-0.5f) == -1, "expected -1");
  check("flt(7)==7.0f", rt_flt(7) == 7.0f, "expected 7.0f");
  check("ord('A')==65", rt_ord('A') == 65, "expected 65");
}

/* ==================================================================== */
/* case 10: nested procedures / static links (R1)                       */
/* ==================================================================== */
static void case_nested_links(void) {
  r1_outer_result = -1;
  r1_outer();                                  /* outer -> middle -> inner */
  check("nested-link-inner-mutates-outer[0 +1(middle) +40(inner) = 41]",
        r1_outer_result == 41,
        "inner's mutation of outer's x not visible through middle's frame");

  r1_plain_calls = 0;
  {
    void (*cmd)(void) = r1_plain;              /* plain command pointer */
    cmd();
    check("nested-plain-callable-as-void-void[no link param]",
          r1_plain_calls == 1, "plain nested procedure not callable as void(void)");
  }

  check("nested-plain-has-plain-signature",
        _Generic(&r1_plain, void (*)(void) : 1, default : 0) != 0,
        "plain nested procedure is not void(*)(void)");
  check("nested-link-not-usable-as-plain-command[typecheck]",
        _Generic(&r1_middle, void (*)(void) : 1, default : 0) == 0,
        "link-taking procedure is (wrongly) a plain void(*)(void)");
  check("nested-link-has-link-signature",
        _Generic(&r1_middle, void (*)(void *) : 1, default : 0) != 0,
        "link-taking procedure is not void(*)(void*)");

  {
    F_outer fo;
    void (*linkfn)(void *);
    fo.x = 0;
    linkfn = (void (*)(void *)) &r1_middle;    /* properly typed indirect call */
    linkfn((void *)&fo);
    check("nested-link-indirect-call-passes-link[0 -> 41]",
          fo.x == 41, "indirect call through link-taking pointer lost the link");
  }
}

/* ==================================================================== */
/* case 11: type CASE via descriptor id (R2)                            */
/* ==================================================================== */
static void case_typecase_id(void) {
  R1 heap1;   /* heap blocks, first word = descriptor pointer */
  R0 heap0;
  R2 heap2;
  R  root;
  heap1.base.desc = &D_R1; heap1.base.x = 10; heap1.z = 22;
  heap0.base.desc = &D_R0; heap0.base.x = 10; heap0.y = 33;
  heap2.base.desc = &D_R2; heap2.base.x = 10; heap2.w = 44;
  root.desc = &D_R; root.x = 10;

  r2_case_ran = 0; r2_r1_field = -1;
  r2_typecase(&heap1);                         /* dynamic type R1 */
  check("typecase-id-R1-arm-runs[TD_R1]", r2_case_ran == 2, "R1 arm did not run");
  check("typecase-id-R0-R2-arms-skipped", r2_case_ran != 1 && r2_case_ran != 3, "wrong arm ran");
  check("typecase-id-arm-narrow-cast[(R1*)v->z]", r2_r1_field == 22, "narrow-cast field wrong");

  r2_case_ran = 0;
  r2_typecase(&heap0);
  check("typecase-id-R0-arm-runs[TD_R0]", r2_case_ran == 1, "R0 arm did not run");
  r2_case_ran = 0;
  r2_typecase(&heap2);
  check("typecase-id-R2-arm-runs[TD_R2]", r2_case_ran == 3, "R2 arm did not run");

  rt_traps = 0;
  r2_case_ran = 0;
  r2_typecase(&root);                          /* id TD_R is not a listed case */
  check("typecase-id-unlisted-hits-default-trap",
        rt_traps == 1 && r2_case_ran == 0, "unlisted descriptor id did not trap");
}

/* ==================================================================== */
/* case 12: array value parameter copied by the caller (R3)             */
/* ==================================================================== */
static void case_array_value_param(void) {
  R3Arr src;
  R3Arr tmp;
  int32_t i;
  src.e[0] = 1; src.e[1] = 2; src.e[2] = 3;
  for (i = 0; i < R3_N; i = i + 1) tmp.e[i] = src.e[i]; /* element-wise copy */
  r3_seen = -1;
  r3_callee(&tmp);
  check("array-value-param-callee-sees-original", r3_seen == 1,
        "callee did not see the original contents");
  check("array-value-param-caller-unchanged",
        src.e[0] == 1 && src.e[1] == 2 && src.e[2] == 3,
        "caller array was mutated through the value parameter");
  check("array-value-param-copy-mutated",
        tmp.e[0] == 99 && tmp.e[1] == 3, "callee did not mutate its copy");
}

/* ==================================================================== */
int main(void) {
  case_divmod();
  case_sets();
  case_for();
  case_strings();
  case_types();
  case_shifts();
  case_open_arrays();
  case_params();
  case_math();
  case_nested_links();
  case_typecase_id();
  case_array_value_param();

  printf("----\n");
  printf("SUMMARY: %d assertion failure(s)\n", (int)failures);
  return failures != 0;
}
