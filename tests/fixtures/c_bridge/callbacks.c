#include "callbacks.h"
#include <stdlib.h>
#include <windows.h>
struct Subscription { Visitor visitor; Event event; void *context; int32_t value; int on_close; };
static int32_t live;
static int32_t dropped;
void note_drop(int32_t count) { dropped++; }
int32_t dropped_handlers(void) { return dropped; }
Subscription *subscribe_fail(Visitor callback, void *context) { return NULL; }
int32_t visit(Visitor callback, void *context, int32_t value) { return callback(value, context); }
int32_t visit_packet(Packet packet, Visitor callback, void *context) { return callback(packet.arguments, context); }
Subscription *subscribe(Visitor callback, void *context) {
    Subscription *s = calloc(1, sizeof(*s));
    if (s) { s->visitor = callback; s->context = context; live++; }
    return s;
}
Subscription *subscribe_events(Event callback, void *context) {
    Subscription *s = calloc(1, sizeof(*s));
    if (s) { s->event = callback; s->context = context; live++; }
    return s;
}
Subscription *subscribe_close(Visitor callback, void *context) {
    Subscription *s = subscribe(callback, context);
    if(s) s->on_close = 1;
    return s;
}
int32_t emit(Subscription *s, int32_t value) {
    if(s->visitor) return s->visitor(value, s->context);
    s->event(value, s->context);
    return 0;
}
static DWORD WINAPI worker(void *arg) {
    Subscription *s = arg;
    emit(s, s->value);
    return 0;
}
void background(Subscription *s, int32_t value) {
    s->value = value;
    HANDLE thread = CreateThread(NULL, 0, worker, s, 0, NULL);
    if(thread) { WaitForSingleObject(thread, INFINITE); CloseHandle(thread); }
}
void unsubscribe(Subscription *s) { if(s->on_close) s->visitor(1, s->context); live--; free(s); }
int32_t live_subscriptions(void) { return live; }
