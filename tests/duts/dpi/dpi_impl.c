/* Reference implementation of the DPI-C imports in dpi.sv.
 * Verilator's generated makefile compiles .c files with the C++ compiler, so the
 * definitions need C linkage explicitly to match the imports. */
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int32_t dpi_add(int32_t x, int32_t y) {
    return (int32_t)((uint32_t)x + (uint32_t)y);
}

/* Returns how many times it has been called, starting at 1. */
int32_t dpi_tick(void) {
    static int32_t count = 0;
    count += 1;
    return count;
}

#ifdef __cplusplus
}
#endif
