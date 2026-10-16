#include "scalars.h"
#include <stdlib.h>
double scalar_mix(int32_t i, float f, bool b, uint64_t u) { return i+f+(b?1:0)+(double)u; }
int64_t scalar_signed(int64_t value) { return value; }
uint64_t scalar_unsigned(uint64_t value) { return value; }
double scalar_float(double value) { return value; }
uint8_t scalar_byte(uint8_t value) { return value; }
void scalar_void(void) {}
int64_t scalar_many(int64_t p0,int64_t p1,int64_t p2,int64_t p3,int64_t p4,int64_t p5,int64_t p6,int64_t p7,int64_t p8,int64_t p9,int64_t p10,int64_t p11) { return 1*p0+2*p1+3*p2+4*p3+5*p4+6*p5+7*p6+8*p7+9*p8+10*p9+11*p10+12*p11; }
