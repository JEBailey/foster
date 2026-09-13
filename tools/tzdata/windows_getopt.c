/* Minimal POSIX getopt for building IANA zic with the Windows C runtime.
 * Foster project license (MIT OR Apache-2.0). Not linked into Foster programs. */
#include <stdio.h>
#include <string.h>
char *optarg;
int optind = 1;
int getopt(int argc, char *const argv[], const char *options) {
    static const char *next;
    if (!next || !*next) {
        if (optind >= argc || argv[optind][0] != '-' || !argv[optind][1]) return -1;
        if (!strcmp(argv[optind], "--")) { ++optind; return -1; }
        next = argv[optind++] + 1;
    }
    int option = *next++;
    const char *found = strchr(options, option);
    if (!found || option == ':') return '?';
    optarg = NULL;
    if (found[1] == ':') {
        if (*next) { optarg = (char *)next; next = NULL; }
        else if (optind < argc) { optarg = argv[optind++]; next = NULL; }
        else return '?';
    }
    return option;
}
