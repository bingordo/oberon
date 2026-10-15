/* rt.h -- the runtime helpers the C back end calls for the predeclared
 * operations of DESIGN.md 9.5.  Hand-written here because the golden fixture
 * needs the real thing, not a re-description of it.
 *
 * Every helper is an ordinary external symbol (the back end links one
 * `ob_runtime`-style object); nothing here is emitted per module.
 */
#ifndef RT_H
#define RT_H

#include <stdint.h>

/* The runtime the *compiler actually ships*, included rather than copied: these
 * cases exist to pin its semantics, and a private copy had already drifted from
 * it once (see DESIGN.md §14). */
#include "../crates/cgen/src/oberon_h.inc"

/* Helpers that the emitted runtime does not need but these vectors do: a
 * NUL-terminated comparison, a bounded copy, and an ordinal read. */
static inline int32_t rt_strcmp(const char *a, const char *b) {
    while (*a != 0 && *a == *b) {
        a = a + 1;
        b = b + 1;
    }
    return (int32_t)(unsigned char)*a - (int32_t)(unsigned char)*b;
}
static inline int32_t rt_strlt(const char *a, const char *b) { return rt_strcmp(a, b) < 0; }
static inline int32_t rt_streq(const char *a, const char *b) { return rt_strcmp(a, b) == 0; }
static inline void rt_charcopy(char *dst, int32_t n, const char *src, int32_t m) {
    int32_t i;
    for (i = 0; i < n && i < m; i++) {
        dst[i] = src[i];
    }
}
static inline int32_t rt_ord(char c) { return (int32_t)(unsigned char)c; }
static inline float rt_flt(int32_t x) { return (float)x; }

#endif /* RT_H */
