#include "fixture.h"
#include <stdlib.h>
#include <string.h>
struct Counter { int64_t value; };
static int64_t live;
int32_t fixture_add(int32_t a, int32_t b) { return (int32_t)((uint32_t)a + (uint32_t)b); }
Counter *fixture_create(int64_t value) {
    if(value == -1) return NULL;
    Counter *result = malloc(sizeof(*result));
    if(result) { result->value = value; live++; }
    return result;
}
int64_t fixture_get(Counter *value) { return value->value; }
void fixture_set(Counter *value, int64_t next) { value->value = next; }
void fixture_destroy(Counter *value) { live--; free(value); }
int fixture_close(Counter *value) {
    if(value->value == -2) return 7;
    fixture_destroy(value);
    return 0;
}
int fixture_close_consumed(Counter *value) {
    int status = value->value < 0 ? 9 : 0;
    fixture_destroy(value);
    return status;
}
int64_t fixture_live(void) { return live; }
uint8_t *fixture_copy(const uint8_t *bytes, size_t length, size_t *result_length) {
    if(!length) { *result_length=0; return NULL; }
    uint8_t *result=malloc(length ? length : 1);
    *result_length = length;
    if(result && length) memcpy(result,bytes,length);
    return result;
}
void fixture_free(uint8_t *bytes) { if(!bytes) abort(); free(bytes); }
double fixture_float(double value) { return value * 2; }
uint64_t fixture_unsigned(uint64_t value) { return value; }
int64_t fixture_string_length(const char *value) { return (int64_t)strlen(value); }
uint8_t fixture_byte(uint8_t value) { return value; }
float fixture_float32(float value) { return value; }
