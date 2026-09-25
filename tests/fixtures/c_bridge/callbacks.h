#include <stdint.h>
typedef int32_t (*Visitor)(int32_t, void *);
typedef void (*Event)(int32_t, void *);
typedef struct Subscription Subscription;
typedef struct Packet { int32_t arguments; } Packet;
int32_t visit(Visitor callback, void *context, int32_t value);
int32_t visit_packet(Packet packet, Visitor callback, void *context);
Subscription *subscribe(Visitor callback, void *context);
Subscription *subscribe_fail(Visitor callback, void *context);
Subscription *subscribe_close(Visitor callback, void *context);
Subscription *subscribe_events(Event callback, void *context);
int32_t emit(Subscription *subscription, int32_t value);
void background(Subscription *subscription, int32_t value);
void unsubscribe(Subscription *subscription);
int32_t live_subscriptions(void);
void note_drop(int32_t count);
int32_t dropped_handlers(void);
