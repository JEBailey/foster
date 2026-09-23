#include <stdint.h>
#include <stddef.h>
typedef struct Counter Counter;
int32_t fixture_add(int32_t a, int32_t b);
Counter *fixture_create(int64_t value);
int64_t fixture_get(Counter *value);
void fixture_set(Counter *value, int64_t next);
void fixture_destroy(Counter *value);
int fixture_close(Counter *value);
int fixture_close_consumed(Counter *value);
int64_t fixture_live(void);
uint8_t *fixture_copy(const uint8_t *bytes, size_t length, size_t *result_length);
void fixture_free(uint8_t *bytes);
double fixture_float(double value);
uint64_t fixture_unsigned(uint64_t value);
int64_t fixture_string_length(const char *value);
uint8_t fixture_byte(uint8_t value);
float fixture_float32(float value);
