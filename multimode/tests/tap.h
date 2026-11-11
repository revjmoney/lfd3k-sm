/* Minimal test harness for the multimode PC tests: CHECK counts, RUN names a
 * test, TAP_DONE returns the failure count from main. No dependencies. */
#ifndef TAP_H
#define TAP_H
#include <stdio.h>

static int tap_checks, tap_fails, tap_tests, tap_test_fails;

#define CHECK(cond)                                                               \
    do {                                                                          \
        tap_checks++;                                                             \
        if (!(cond)) {                                                            \
            tap_fails++;                                                          \
            tap_test_fails++;                                                     \
            printf("    FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);            \
        }                                                                         \
    } while (0)

#define CHECK_EQ(a, b)                                                            \
    do {                                                                          \
        long long tap_a = (long long)(a), tap_b = (long long)(b);                 \
        tap_checks++;                                                             \
        if (tap_a != tap_b) {                                                     \
            tap_fails++;                                                          \
            tap_test_fails++;                                                     \
            printf("    FAIL %s:%d: %s == %s (got %lld, want %lld)\n", __FILE__,  \
                   __LINE__, #a, #b, tap_a, tap_b);                               \
        }                                                                         \
    } while (0)

#define RUN(fn)                                                                   \
    do {                                                                          \
        tap_test_fails = 0;                                                       \
        tap_tests++;                                                              \
        fn();                                                                     \
        printf("%s %s\n", tap_test_fails ? "not ok" : "ok    ", #fn);             \
    } while (0)

#define TAP_DONE()                                                                \
    do {                                                                          \
        printf("%d tests, %d checks, %d failed checks\n", tap_tests, tap_checks,  \
               tap_fails);                                                        \
        return tap_fails ? 1 : 0;                                                 \
    } while (0)

#endif
