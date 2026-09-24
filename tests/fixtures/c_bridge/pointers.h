#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
typedef struct Pair { int32_t x, y; } Pair;
typedef struct OwnedValue { int32_t *data; } OwnedValue;
const char *borrowed_text(void);
const char *null_text(void);
const char *unterminated_text(void);
char *owned_text(void);
void release_text(char *text);
uint8_t *owned_bytes(int32_t *length);
uint8_t *bad_bytes(int32_t *length);
void release_bytes(uint8_t *data);
int32_t sum_bytes(const uint8_t *data, int32_t length);
int32_t mutate_bytes(uint8_t *data, int32_t length);
int32_t parse_outputs(const char *text, int32_t *count, Pair *pair);
void update_pair(Pair *pair);
void single_output(int32_t *value);
OwnedValue create_value(int32_t value);
OwnedValue value_from_bytes(const uint8_t *data, int32_t length);
bool valid_value(OwnedValue value);
int32_t read_value(OwnedValue value);
void destroy_value(OwnedValue value);
int32_t live_values(void);
int32_t released_buffers(void);
void fill_bytes(uint8_t *data, int32_t capacity);
int32_t sum_pairs(const Pair *data, int32_t length);
void mutate_pairs(Pair *data, int32_t length);
void fill_pairs(Pair *data, int32_t capacity);
void set_value(OwnedValue *value, int32_t number);
