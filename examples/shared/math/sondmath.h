/* C / C++ header for libsondmath.so (Sond shared library). */
#ifndef SONDMATH_H
#define SONDMATH_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {           /* Sond exports unmangled C symbols; this silences C++ mangling */
#endif

int64_t sond_add(int64_t a, int64_t b);
int64_t sond_mul(int64_t a, int64_t b);
int64_t sond_fib(int64_t n);
int64_t sond_answer(int64_t x);

#ifdef __cplusplus
}
#endif
#endif /* SONDMATH_H */
