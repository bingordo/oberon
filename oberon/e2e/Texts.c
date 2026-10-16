/* A hand-written Texts fixture in the emitted shape lets the report's own Out
 * module compile and run against it unchanged. */
#include "Texts.h"
#include <stdio.h>

const ObDesc TD_Texts__Writer = { NULL, 9001u, "Texts.Writer", NULL, 0 };

void Texts__OpenWriter(Texts__Writer *w) {
    w->desc = &TD_Texts__Writer;
    rt_strcopy(w->buf.e, 256, "");
}

void Texts__Write(Texts__Writer *w, char ch) {
    int32_t n = (int32_t)strlen(w->buf.e);
    if (n < 255) {
        w->buf.e[n] = ch;
        w->buf.e[n + 1] = 0;
    }
}

void Texts__WriteString(Texts__Writer *w, char *s, int32_t s__len) {
    int32_t i;
    for (i = 0; i < s__len && s[i] != 0; i++) {
        Texts__Write(w, s[i]);
    }
}

void Texts__WriteLn(Texts__Writer *w) {
    printf("%s\n", w->buf.e);
    rt_strcopy(w->buf.e, 256, "");
}

/* Append copies the writer's buffer to the log. In this fixture the log is
 * standard output and WriteLn has already flushed the line, so this only
 * clears the buffer (printing here would double every line). */
void Texts__Append(Texts__Writer *w, char *b, int32_t b__len) {
    (void)b;
    (void)b__len;
    (void)w;
}

void Texts__init(void) {}
void Texts__load(void) { Texts__init(); }
