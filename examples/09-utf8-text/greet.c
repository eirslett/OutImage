#include <stdint.h>
#include <stdio.h>

void greet(const uint8_t *p, int64_t n) {
    fwrite(p, 1, (size_t)n, stdout);
    fputc('\n', stdout);
}
