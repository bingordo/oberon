/* oberon.h -- runtime interface for the C emitted by the Oberon-07 compiler.
 *
 * Every helper here is `static inline`, so a translation unit includes exactly
 * the ones it calls and an unused helper never trips -Wunused-function.
 * The few things that must be shared across modules (the type descriptors'
 * identity and the command registry) have external linkage and live in
 * oberon_runtime.c.
 *
 * The semantics of these helpers are the ones verified in oberon/csmoke and
 * oberon/golden: floor DIV/MOD with the INT32_MIN/-1 case, shifts with the count
 * reduced mod 32, sets as 32-bit masks, bounded CHAR-array comparison.
 */
#ifndef OBERON_H
#define OBERON_H

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* A type descriptor. Records that can be extended keep one in their first word
 * (a record variable's own, or the one stored in a heap block for `POINTER TO`),
 * so `IS`, a type guard and a type CASE all walk `base` (report 6.3, 6.4). */
typedef struct ObDesc ObDesc;
struct ObDesc {
    const ObDesc *base;
    int32_t id;
    const char *name;
};

/* A parameterless exported procedure, callable by name (report 11). */
typedef struct ObCommand ObCommand;
struct ObCommand {
    const char *name;
    void (*fn)(void);
};

void ob_register_commands(const char *module, const ObCommand *table);
int ob_run_command(const char *module, const char *name);

/* ------------------------------------------------------------------ traps */

static inline void rt_trap_pos(const char *msg, int32_t at) {
    extern void ob_trap_report_at(const char *msg, int32_t at);
    ob_trap_report_at(msg, at);
    abort();
}

static inline void rt_trap(const char *msg) {
    rt_trap_pos(msg, -1);
}

static inline void rt_guard(int32_t cond, const char *msg) {
    if (!cond) rt_trap(msg);
}

/* Bounds checks carry the source offset of the designator, which is what makes
 * a trap report locatable without a debugger (DESIGN 9.4). */
static inline int32_t rt_index_at(int32_t i, int32_t n, int32_t at) {
    if (i < 0 || i >= n) {
        rt_trap_pos("index out of range", at);
    }
    return i;
}

#define rt_index(i, n, at) rt_index_at((i), (n), (at))

/* --------------------------------------------------------------- integers */

/* x DIV y = floor(x/y); x MOD y = x - (x DIV y)*y.  For y > 0 that is the
 * report's `0 <= r < y` (report 8.2.2); for y < 0 it floors, which is how
 * every Oberon implementation behaves.  y == -1 is special-cased because the
 * hardware traps on INT32_MIN / -1 and the wrap is what two's complement says. */
static inline int32_t rt_div(int32_t x, int32_t y) {
    if (y == 0) rt_trap("DIV by zero");
    if (y == -1) return (int32_t)(0u - (uint32_t)x);
    int32_t q = x / y;
    int32_t r = x % y;
    if (r != 0 && (r < 0) != (y < 0)) q = q - 1;
    return q;
}

static inline int32_t rt_mod(int32_t x, int32_t y) {
    if (y == 0) rt_trap("MOD by zero");
    if (y == -1) return 0;
    int32_t r = x % y;
    if (r != 0 && (r < 0) != (y < 0)) r = r + y;
    return r;
}

/* The report's table (§8.2.2) defines these as `x * 2^n` and `x DIV 2^n`, so a
 * count of 32 or more is defined rather than a no-op: multiplying by 2^32 wraps
 * to zero, and `x DIV 2^32` is 0 or -1. Only a *negative* count is undefined,
 * and masking it keeps the C defined too (R22). */
static inline int32_t rt_lsl(int32_t x, int32_t n) {
    if (n >= 32) return 0;
    return (int32_t)((uint32_t)x << ((uint32_t)n & 31u));
}

/* Portable arithmetic shift right: the sign is re-extended explicitly, so this
 * does not depend on `>>` being arithmetic (DESIGN 9.5). */
static inline int32_t rt_asr(int32_t x, int32_t n) {
    if (n >= 32) return (x < 0) ? -1 : 0;
    uint32_t k = (uint32_t)n & 31u;
    return (x < 0) ? (int32_t)~(~(uint32_t)x >> k) : (int32_t)((uint32_t)x >> k);
}

static inline int32_t rt_ror(int32_t x, int32_t n) {
    return (int32_t)((uint32_t)x >> ((uint32_t)n & 31u) |
                     (uint32_t)x << ((32u - ((uint32_t)n & 31u)) & 31u));
}

/* `ABS(x)` takes its operand once: written as a conditional expression the
 * operand would appear three times and a call in it would run twice. */
static inline int32_t rt_abs(int32_t x) {
    return (x < 0) ? -x : x;
}

/* `ABS` is defined for REAL as well and keeps the operand's type (report
 * §10.2), so REAL gets its own helper rather than an int conversion. */
static inline float rt_absf(float x) {
    return (x < 0.0f) ? -x : x;
}

static inline float rt_floor(float x) {
    int32_t t = (int32_t)x;
    return (float)((x < (float)t) ? (t - 1) : t);
}

/* --------------------------------------------------------------- sets */

/* {a .. b} as a mask; empty when a > b, and the width-32 case shifts by a. */
static inline uint32_t rt_set(int32_t a, int32_t b) {
    if (a < 0) a = 0;
    if (b > 31) b = 31;
    if (a > b) return 0u;
    if (b == 31) return 0xFFFFFFFFu << (uint32_t)a;
    return ((1u << (uint32_t)(b - a + 1)) - 1u) << (uint32_t)a;
}

static inline int32_t rt_in(uint32_t s, int32_t e) {
    return (int32_t)((s >> ((uint32_t)e & 31u)) & 1u);
}

/* --------------------------------------------------------------- strings */

/* A string literal into an ARRAY n OF CHAR: length-checked by the compiler,
 * zero-padded here, so the array is NUL-terminated (report 9.1.2). */
static inline void rt_strcopy(char *dst, int32_t n, const char *src) {
    int32_t i = 0;
    while (i < n && src[i] != 0) {
        dst[i] = src[i];
        i = i + 1;
    }
    while (i < n) {
        dst[i] = 0;
        i = i + 1;
    }
}

/* Character arrays are compared up to the shorter operand and stop at a NUL in
 * either - an ARRAY n OF CHAR need not be NUL-terminated after a full-length
 * array-to-array copy (DESIGN 9.5, risk R14). */
static inline int32_t rt_charcmp(const char *a, int32_t na, const char *b, int32_t nb) {
    int32_t i;
    for (i = 0; i < na || i < nb; i++) {
        /* The byte after an operand's length counts as NUL, so a filled array
         * and a NUL-terminated one with the same content compare equal, and
         * the result agrees with strcmp wherever both are terminated. */
        unsigned char ca = (i < na) ? (unsigned char)a[i] : 0;
        unsigned char cb = (i < nb) ? (unsigned char)b[i] : 0;
        if (ca != cb) return (int32_t)ca - (int32_t)cb;
        if (ca == 0) return 0;
    }
    return 0;
}

/* ------------------------------------------------------------- records */

/* The descriptor of the record a designator denotes: pass the payload pointer
 * for a POINTER, the address of the record for a record variable. */
static inline const ObDesc *rt_dyn(const void *p) {
    return *(const ObDesc *const *)p;
}

static inline int32_t rt_is(const ObDesc *d, const ObDesc *target) {
    while (d != NULL) {
        if (d == target) return 1;
        d = d->base;
    }
    return 0;
}

/* NEW(p): the block carries the descriptor of p's static base type in its first
 * word, and failure yields NULL, which is what the report says (6.4). */
static inline void *rt_new(int32_t size, const ObDesc *d) {
    void *p;
    if (size < (int32_t)sizeof(const ObDesc *)) size = (int32_t)sizeof(const ObDesc *);
    p = malloc((size_t)size);
    if (p == NULL) return NULL;
    *(const ObDesc **)p = d;
    return p;
}

/* ------------------------------------------------------------- PACK/UNPK */

/* PACK(x, n): x := x * 2^n.  `n` is a value here, not a result: the report
 * says "PACK(x, y) is equivalent to x := x * 2^y" (10.2). */
static inline void rt_pack(float *x, int32_t n) {
    while (n > 0) {
        *x = *x * 2.0f;
        n = n - 1;
    }
    while (n < 0) {
        *x = *x / 2.0f;
        n = n + 1;
    }
}

/* UNPK(x, n): split x into a mantissa in [1, 2) and an exponent, with the sign
 * kept on the mantissa (10.2).  Both loops are bounded, so an infinity or a
 * denormal cannot spin. */
static inline void rt_unpk(float *x, int32_t *n) {
    float v = *x;
    int32_t e = 0;
    int32_t neg = 0;
    if (v < 0.0f) {
        v = 0.0f - v;
        neg = 1;
    }
    for (;;) {
        if (v >= 2.0f && e < 16384) {
            v = v / 2.0f;
            e = e + 1;
        } else if (v < 1.0f && v > 0.0f && e > -16384) {
            v = v * 2.0f;
            e = e - 1;
        } else {
            break;
        }
    }
    *x = neg ? (0.0f - v) : v;
    *n = e;
}

/* A guard's test, usable as an expression so that it keeps its place in a
 * short-circuit operand: `(rt_guard_is(cond, msg), value)`. */
/* A pointer access is checked for NIL. The report makes using NIL an error
 * (§6.4) and the design traps rather than faulting (R21), with the source offset
 * of the designator so a failure is locatable without a debugger.
 *
 * A macro rather than a helper function, and not because of speed: the result has
 * to keep the operand's *pointer type* (a `void *` could not be selected from),
 * and `rt_nil(p, at)->f` must remain an lvalue so an assignment to a field of a
 * pointer target still compiles. The operand is a variable or a field, never a
 * call, so evaluating it twice is free. */
#define rt_nil(p, at) \
    (((p) == NULL) ? ((rt_trap_pos("NIL pointer dereference", (at))), (p)) : (p))

static inline void rt_guard_is(int32_t cond, const char *msg) {
    if (!cond) rt_trap(msg);
}

/* Copy a record value into a destination of the *same or a base* type: the
 * fields move, but the destination keeps its own static descriptor, so a base
 * variable cannot advertise a derived dynamic type (DESIGN 9.3). */
static inline void rt_rec_copy(void *dst, const void *src, int32_t n, const ObDesc *self) {
    if (dst != src) memmove(dst, src, (size_t)n);
    *(const ObDesc **)dst = self;
}

#endif /* OBERON_H */
