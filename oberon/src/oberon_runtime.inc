/* oberon_runtime.c -- the part of the runtime that must be shared by every
 * generated module: the heap and its collector, the trap reporter and the
 * command registry (report 11).
 *
 * Link this once with the generated translation units.
 */
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <stdint.h>

#include "oberon.h"

/* --------------------------------------------------------------- the heap */

/* The collector's header, in front of the payload. The payload pointer is still
 * the object -- its first word is still its descriptor, which is what `IS`, a
 * guard and a type CASE read (DESIGN 9.3) -- and the header is what gives the
 * sweep the next block and the size to free. A separate table would have to be
 * kept in step with the heap for the same two facts, and would still not say
 * which block an interior pointer belongs to.
 *
 * `sizeof(ObBlock)` is a multiple of `sizeof(void *)`, so the payload is as
 * aligned as `malloc` alone would have made it for every type the back end
 * emits: int32_t, char, float, uint32_t and pointers are the widest (DESIGN
 * 9.2). */
typedef struct ObBlock ObBlock;
struct ObBlock {
    ObBlock *next;
    size_t size;
    int32_t mark;
};

static ObBlock *ob_blocks = NULL;    /* every block, newest first */
static size_t ob_heap_bytes = 0;     /* what the heap holds, headers included */
static uintptr_t ob_heap_lo = UINTPTR_MAX; /* the range a candidate must fall */
static uintptr_t ob_heap_hi = 0;           /* in to be a pointer at all */

/* The three things that turn the collector on; all of them are off in a normal
 * run, which is what lets the rest of the suite pass unchanged (DESIGN 15.5,
 * 15.8, 15.9).
 * `OB_GC_STRESS` collects before every allocation, so a suite sees the
 * collector work without arranging a heap small enough to fail. `OB_GC_LIMIT`
 * is a hard bound on the bytes the heap may hold -- 0, like an unset variable,
 * means no bound -- and it is the second trigger: the first is `malloc` itself
 * failing, and this one is what makes the report's `NEW`-yields-`NIL` (report
 * 6.4) reachable with a bounded heap. */
static int ob_stress = 0;
static size_t ob_limit = 0;
static int ob_env_read = 0;

static void ob_gc_env(void) {
    const char *stress;
    const char *limit;
    if (ob_env_read) return;
    ob_env_read = 1;
    stress = getenv("OB_GC_STRESS");
    ob_stress = stress != NULL && atoi(stress) != 0;
    limit = getenv("OB_GC_LIMIT");
    if (limit != NULL) ob_limit = (size_t)strtoul(limit, NULL, 10);
}

/* One module's roots, as `__init` hands them over (DESIGN 15.4). The list grows:
 * a module has as many roots as it has variables that can reach the heap, and
 * the collector must not impose a limit the source language does not have. */
struct ObRoot {
    const void *base;
    const ObFld *fields;
    int32_t n;
};

static struct ObRoot *ob_roots = NULL;
static int32_t ob_roots_len = 0;
static int32_t ob_roots_cap = 0;

void ob_gc_root_region(const void *base, const ObFld *fields, int32_t n) {
    struct ObRoot *grown;
    if (n <= 0) return;
    if (ob_roots_len == ob_roots_cap) {
        ob_roots_cap = ob_roots_cap == 0 ? 16 : ob_roots_cap * 2;
        grown = (struct ObRoot *)realloc(ob_roots, (size_t)ob_roots_cap * sizeof *grown);
        if (grown == NULL) rt_trap("out of memory registering roots");
        ob_roots = grown;
    }
    ob_roots[ob_roots_len].base = base;
    ob_roots[ob_roots_len].fields = fields;
    ob_roots[ob_roots_len].n = n;
    ob_roots_len = ob_roots_len + 1;
}

/* The mark stack: an explicit work list rather than recursion, because the
 * objects a program links together can be as long a chain as it can allocate,
 * and marking one recursively would exhaust the C stack instead of the heap. */
static const char **ob_mark = NULL;
static size_t ob_mark_len = 0;
static size_t ob_mark_cap = 0;

static void ob_mark_push(const char *obj) {
    const char **grown;
    if (ob_mark_len == ob_mark_cap) {
        ob_mark_cap = ob_mark_cap == 0 ? 64 : ob_mark_cap * 2;
        grown = (const char **)realloc(ob_mark, ob_mark_cap * sizeof *grown);
        if (grown == NULL) rt_trap("out of memory marking the heap");
        ob_mark = grown;
    }
    ob_mark[ob_mark_len] = obj;
    ob_mark_len = ob_mark_len + 1;
}

/* A word that looks like a heap address marks its block. An interior pointer --
 * one that lands inside a block rather than on its first byte, as an `ADR` of a
 * field gives -- marks the block it lies in, because the block is what the
 * sweep can free and what the block's own map then walks (DESIGN 15.3). A
 * pointer outside every block, a code address, a NIL: nothing. */
static void ob_mark_value(const void *value) {
    uintptr_t v = (uintptr_t)value;
    ObBlock *b;
    if (v < ob_heap_lo || v >= ob_heap_hi) return;
    for (b = ob_blocks; b != NULL; b = b->next) {
        uintptr_t start = (uintptr_t)(b + 1);
        if (v >= start && v < start + b->size) {
            if (!b->mark) {
                b->mark = 1;
                ob_mark_push((const char *)(b + 1));
            }
            return;
        }
    }
}

/* Walk a region's map: a pointer slot is a candidate, a run of
 * descriptor-fronted records -- a record field, an ARRAY OF RECORD -- is walked
 * with the map of the type the descriptor in the map names (DESIGN 15.2). The
 * map is the *static* type's, which is the whole of an embedded record: only a
 * `NEW`-allocated object is reached through the descriptor its block carries. */
static void ob_scan_fields(const char *base, const ObFld *f, int32_t n) {
    int32_t i;
    for (i = 0; i < n; i++) {
        size_t k;
        for (k = 0; k < f[i].n; k++) {
            const char *at = base + f[i].off + k * f[i].stride;
            if (f[i].desc == NULL) {
                ob_mark_value(*(const void *const *)at);
            } else {
                ob_scan_fields(at, f[i].desc->fields, f[i].desc->nfields);
            }
        }
    }
}

/* A heap object: the descriptor is at offset 0 of the payload, whatever the
 * view it is reached through (DESIGN 9.3, 9.4). An object whose descriptor is
 * NULL -- only a program that wrote one, through SYSTEM -- keeps its fields and
 * is retained, which is the safe direction. */
static void ob_scan_object(const char *obj) {
    const ObDesc *d = rt_dyn(obj);
    if (d != NULL) ob_scan_fields(obj, d->fields, d->nfields);
}

/* Under -fsanitize=address the walk reads words in frames the program does not
 * name, which is the point of a conservative scan, and ASan poisons the space
 * around those frames' variables: unpoisoning the region is what lets the walk
 * run under the sanitizer. ASan also keeps locals off the stack by default, in
 * a "fake stack" the walk cannot see, so the collector turns that off in a
 * constructor that runs before the one capturing the stack top -- the option is
 * read when a frame is created, so it has to be off before the frame that holds
 * the top exists. Without a sanitizer, neither is compiled. */
#if defined(__SANITIZE_ADDRESS__)
#define OB_ASAN 1
#elif defined(__has_feature)
#if __has_feature(address_sanitizer)
#define OB_ASAN 1
#endif
#endif

#ifdef OB_ASAN
void __asan_unpoison_memory_region(const void *addr, size_t size);
extern int __asan_option_detect_stack_use_after_return;
#define OB_UNPOISON(addr, size) __asan_unpoison_memory_region((addr), (size))
#else
#define OB_UNPOISON(addr, size) ((void)0)
#endif

/* The stack base, captured before `main` runs. This is the one place a C program
 * can stand to see the whole stack: a constructor's frame is above every frame
 * the program itself will use, so the words from there down to the collector's
 * own frame are exactly the frames that are live when it runs. It is kept as a
 * bare address rather than a `char *`, because nothing is ever a `char` there:
 * the words at it are read as words (`-Wdangling-pointer` is right about the
 * pointer spelling, and this one is honest about what is kept). */
static uintptr_t ob_stack_top = 0;

#ifdef OB_ASAN
__attribute__((constructor(101))) static void ob_real_stack(void) {
    __asan_option_detect_stack_use_after_return = 0;
}
#endif

__attribute__((constructor(102))) static void ob_capture_stack_top(void) {
    char here;
    ob_stack_top = (uintptr_t)&here;
}

/* The conservative half of the root set (DESIGN 15.3): the stack and the
 * registers the frames hold. A C program cannot name the values a callee-saved
 * register carries across a call, so the collector asks `setjmp` for a snapshot
 * and puts it where the walk can see it -- those registers land in this frame,
 * and this frame is part of the region. Every word in between is a candidate: a
 * stale one can only retain an object, and retaining is the one error a
 * reclaimer is allowed to make. */
static void ob_mark_stack(void) {
    jmp_buf regs;
    uintptr_t lo;
    uintptr_t hi;
    uintptr_t w;
    char here;
    (void)setjmp(regs);
    lo = (uintptr_t)&regs;
    if ((uintptr_t)&here < lo) lo = (uintptr_t)&here;
    hi = ob_stack_top;
    if (hi <= lo) return;
    lo = lo & ~(uintptr_t)(sizeof(uintptr_t) - 1);
    OB_UNPOISON((const void *)lo, hi - lo);
    for (w = lo; w + sizeof(uintptr_t) <= hi; w += sizeof(uintptr_t)) {
        uintptr_t word;
        /* `memcpy` rather than a pointer cast: the word may be a `float`, a
         * field of a record or a saved register, and reading it as an object of
         * another type is exactly what a conservative scan must not assume. */
        memcpy(&word, (const void *)w, sizeof word);
        ob_mark_value((const void *)word);
    }
}

/* Mark, then sweep. The two phases are what make this a reclaimer that never
 * frees a live object: what the mark did not reach was unreachable when the
 * roots were read, and nothing moves, so a pointer a program still holds is
 * still valid (DESIGN 15.1, 15.6). */
static void ob_collect(void) {
    ObBlock **link;
    if (ob_blocks == NULL) return; /* an empty heap has nothing to reclaim */
    ob_mark_len = 0;
    {
        int32_t i;
        for (i = 0; i < ob_roots_len; i++) {
            ob_scan_fields((const char *)ob_roots[i].base, ob_roots[i].fields, ob_roots[i].n);
        }
    }
    ob_mark_stack();
    while (ob_mark_len > 0) {
        ob_mark_len = ob_mark_len - 1;
        ob_scan_object(ob_mark[ob_mark_len]);
    }
    link = &ob_blocks;
    while (*link != NULL) {
        ObBlock *b = *link;
        if (b->mark) {
            b->mark = 0; /* the flag is this collection's, not the block's */
            link = &b->next;
        } else {
            *link = b->next;
            ob_heap_bytes -= sizeof(ObBlock) + b->size;
            free(b);
        }
    }
}

/* What is still on the heap at exit is what the program still holds: one last
 * collection there is what lets a leak checker that runs after `atexit` -- the
 * sanitizers' does -- agree with the collector instead of reporting the
 * collector's work as a leak. It is registered by the first allocation, so a
 * program that never allocates never pays for it. */
static void ob_gc_at_exit(void) {
    ob_collect();
}

/* `NEW`: one allocation point for the whole language, so a failing allocation
 * has one answer -- the pointer becomes `NIL` (report 6.4) -- and the collector
 * has one place to hook. */
void *rt_new(size_t size, const ObDesc *d) {
    void *p;
    ObBlock *b;
    static int32_t allocations = 0;
    static int at_exit = 0;
    const char *fail_at = getenv("OB_ALLOC_FAIL_AT");
    allocations += 1;
    if (fail_at != NULL && atoi(fail_at) == allocations) return NULL;
    ob_gc_env();
    /* A record may be larger than any 32-bit extent and `NEW` passes
     * `sizeof(T)`: the size survives the call, so it is `size_t` here rather
     * than narrowed to the word the descriptor records. */
    if (size < sizeof(const ObDesc *)) size = sizeof(const ObDesc *);
    if (!at_exit) {
        at_exit = 1;
        atexit(ob_gc_at_exit);
    }
    /* Collect before giving up, so that the report's failure case keeps its
     * meaning with a bounded heap and `OB_ALLOC_FAIL_AT` stays a test hook:
     * the attempt ends in `NIL` only when the program's own reachable data no
     * longer fits (DESIGN 15.5). With `OB_GC_LIMIT` set, "fits" means the limit
     * as well as `malloc`. */
    if (ob_stress || (ob_limit != 0 && ob_heap_bytes + sizeof(ObBlock) + size > ob_limit)) {
        ob_collect();
    }
    if (ob_limit != 0 && ob_heap_bytes + sizeof(ObBlock) + size > ob_limit) return NULL;
    b = (ObBlock *)malloc(sizeof(ObBlock) + size);
    if (b == NULL && ob_blocks != NULL) {
        /* `malloc` failed under a heap this program believes is bounded: one
         * collection, then one more try. Never a loop -- a collection that frees
         * nothing ends the attempt, which is the `NIL` the report specifies. */
        ob_collect();
        b = (ObBlock *)malloc(sizeof(ObBlock) + size);
    }
    if (b == NULL) return NULL;
    b->size = size;
    b->mark = 0;
    b->next = ob_blocks;
    ob_blocks = b;
    ob_heap_bytes += sizeof(ObBlock) + size;
    p = (void *)(b + 1);
    if ((uintptr_t)p < ob_heap_lo) ob_heap_lo = (uintptr_t)p;
    if ((uintptr_t)p + size > ob_heap_hi) ob_heap_hi = (uintptr_t)p + size;
    *(const ObDesc **)p = d;
    return p;
}

/* One report line per trap, then abort: a failed guard, a bounds violation, a
 * NIL dereference or a CASE with no match (DESIGN 9.4). */
void ob_trap_report_at(const char *msg, int32_t at) {
    fflush(stdout);
    if (at < 0) {
        fprintf(stderr, "oberon: trap: %s\n", msg);
    } else {
        fprintf(stderr, "oberon: trap: %s (at byte %d of the source)\n", msg, (int)at);
    }
}

/* Commands, as report 11 defines them: parameterless procedures of a module,
 * activatable by name once the module is loaded. A small fixed table is enough
 * for a command-oriented system and keeps the runtime allocation-free. */
#define OB_MAX_MODULES 32
#define OB_MAX_COMMANDS 64

struct ObRegistry {
    const char *module;
    const ObCommand *table;
};

static struct ObRegistry ob_registry[OB_MAX_MODULES];
static int ob_registry_len = 0;

void ob_register_commands(const char *module, const ObCommand *table) {
    int i;
    for (i = 0; i < ob_registry_len; i++) {
        if (strcmp(ob_registry[i].module, module) == 0) {
            ob_registry[i].table = table;
            return;
        }
    }
    if (ob_registry_len == OB_MAX_MODULES) {
        ob_trap_report_at("too many modules registered", -1);
        abort();
    }
    ob_registry[ob_registry_len].module = module;
    ob_registry[ob_registry_len].table = table;
    ob_registry_len = ob_registry_len + 1;
}

int ob_run_command(const char *module, const char *name) {
    int i;
    for (i = 0; i < ob_registry_len; i++) {
        if (strcmp(ob_registry[i].module, module) == 0) {
            const ObCommand *c = ob_registry[i].table;
            for (; c != NULL && c->name != NULL; c++) {
                if (strcmp(c->name, name) == 0) {
                    c->fn();
                    return 1;
                }
            }
            return 0;
        }
    }
    return 0;
}

/* -------------------------------------------------- the object boundary */

/* The Cranelift back end's entry points (DESIGN 16.2). Every check the emitted
 * C makes is a `static inline` helper in the header, which is free for the C
 * compiler and invisible to a linker; a generated object needs the symbol, and
 * these are it. Each one calls its helper, so there is one implementation of
 * each rule for both back ends -- the arithmetic in the header, and the message
 * and the source byte with it. */
int32_t ob_rt_index_at(int32_t i, int32_t n, int32_t at) {
    return rt_index_at(i, n, at);
}

int32_t ob_rt_div_at(int32_t x, int32_t y, int32_t at) {
    return rt_div_checked(x, y, at);
}

int32_t ob_rt_mod_at(int32_t x, int32_t y, int32_t at) {
    return rt_mod_checked(x, y, at);
}

void ob_rt_trap_pos(const char *msg, int32_t at) {
    rt_trap_pos(msg, at);
}
