#include "borrowed.h"
int64_t sum_i64(const int64_t *values, size_t count) {
    int64_t result = 0;
    for (size_t i = 0; i < count; ++i) result += values[i];
    return result;
}
uint64_t first_u64(const uint64_t *values, int32_t count) { return count ? values[0] : 0; }
double sum_f64(const double *values, uint32_t count) {
    double result = 0;
    for (size_t i = 0; i < count; ++i) result += values[i];
    return result;
}
int64_t sum_u8(const uint8_t *values, size_t count) {
    int64_t result = 0;
    for (size_t i = 0; i < count; ++i) result += values[i];
    return result;
}
uint64_t address_i64(const int64_t *values, size_t count) {
    (void)count;
    return (uint64_t)(uintptr_t)values;
}
int64_t mixed_u8(int64_t before, const uint8_t *values, size_t count, int64_t after) {
    return before * 1000 + sum_u8(values, count) * 10 + after;
}
