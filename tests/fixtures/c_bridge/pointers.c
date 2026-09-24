#include "pointers.h"
#include <stdlib.h>
#include <string.h>
static int32_t live, released;
const char *borrowed_text(void) { return "caf\xc3\xa9"; }
const char *null_text(void) { return NULL; }
const char *unterminated_text(void) { static const char data[4]={'a','b','c','d'}; return data; }
char *owned_text(void) { char *p=malloc(3); if(p) memcpy(p,"ok",3); return p; }
void release_text(char *p) { free(p); released++; }
uint8_t *owned_bytes(int32_t *length) { *length=3; uint8_t *p=malloc(3); if(p) { p[0]=0; p[1]=42; p[2]=255; } return p; }
uint8_t *bad_bytes(int32_t *length) { *length=-1; return malloc(1); }
void release_bytes(uint8_t *p) { free(p); released++; }
int32_t sum_bytes(const uint8_t *p,int32_t n) { int32_t sum=0; for(int32_t i=0;i<n;i++) sum+=p[i]; return sum; }
int32_t mutate_bytes(uint8_t *p,int32_t n) { for(int32_t i=0;i<n;i++) p[i]^=255; return n; }
int32_t parse_outputs(const char *text,int32_t *count,Pair *pair) { *count=(int32_t)strlen(text); pair->x=20; pair->y=22; return 7; }
void update_pair(Pair *pair) { pair->x+=2; pair->y*=2; }
void single_output(int32_t *value) { *value=42; }
OwnedValue create_value(int32_t value) { OwnedValue result={0}; if(value<0) return result; result.data=malloc(sizeof(*result.data)); if(result.data) { *result.data=value; live++; } return result; }
OwnedValue value_from_bytes(const uint8_t *data,int32_t length) { return create_value(sum_bytes(data,length)); }
bool valid_value(OwnedValue value) { return value.data!=NULL; }
int32_t read_value(OwnedValue value) { return *value.data; }
void destroy_value(OwnedValue value) { if(value.data) { free(value.data); live--; } }
int32_t live_values(void) { return live; }
int32_t released_buffers(void) { return released; }
void fill_bytes(uint8_t *data, int32_t capacity) { if(capacity) data[0]=42; }
int32_t sum_pairs(const Pair *data, int32_t length) { int32_t sum=0; for(int32_t i=0;i<length;i++) sum+=data[i].x+data[i].y; return sum; }
void mutate_pairs(Pair *data, int32_t length) { for(int32_t i=0;i<length;i++) data[i].x+=1; }
void fill_pairs(Pair *data, int32_t capacity) { if(capacity) { data[0].x=20; data[0].y=22; } }
void set_value(OwnedValue *value, int32_t number) { *value->data=number; }
