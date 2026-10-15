#include <stdint.h>
#include <stddef.h>
int64_t sum_i64(const int64_t *values, size_t count);
uint64_t first_u64(const uint64_t *values, int32_t count);
double sum_f64(const double *values, uint32_t count);
int64_t sum_u8(const uint8_t *values, size_t count);
uint64_t address_i64(const int64_t *values, size_t count);
int64_t mixed_u8(int64_t before, const uint8_t *values, size_t count, int64_t after);
