/* Hand-written, in the shape the emitter produces for Texts.sym. */
#ifndef TEXTS_H
#define TEXTS_H
#include "oberon.h"

typedef struct Texts__Buffer Texts__Buffer;
struct Texts__Buffer { char e[256]; };
typedef struct Texts__Writer Texts__Writer;
struct Texts__Writer { const ObDesc *desc; Texts__Buffer buf; };

extern const ObDesc TD_Texts__Writer;

void Texts__OpenWriter(Texts__Writer *w);
void Texts__Write(Texts__Writer *w, char ch);
void Texts__WriteString(Texts__Writer *w, char *s, int32_t s__len);
void Texts__WriteLn(Texts__Writer *w);
void Texts__Append(Texts__Writer *w, char *b, int32_t b__len);
void Texts__init(void);
void Texts__load(void);
#endif
