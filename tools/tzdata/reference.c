/* Independent offset oracle using the pinned IANA localtime implementation.
 * Foster controls inputs, ordering, formatting, and fixture publication. */
#include "private.h"

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    timezone_t zone = tzalloc(argv[1]);
    if (!zone) return 3;
    for (int index = 2; index < argc; ++index) {
        char *end;
        errno = 0;
        int64_t epoch = strtoll(argv[index], &end, 10);
        if (errno || *end) return 4;
        time_t instant = epoch;
        struct tm local;
        if (!localtime_rz(zone, &instant, &local)) return 5;
        /* timegm treats these local calendar fields as UTC, giving their offset
         * without requiring the host struct tm to expose tm_gmtoff. */
        time_t wall = timegm(&local);
        printf("%" PRId64 "\n", (int64_t)wall - epoch);
    }
    tzfree(zone);
    return 0;
}
