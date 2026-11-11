/* PC test stub. */
#ifndef FAKE_FREERTOS_H
#define FAKE_FREERTOS_H
typedef struct { int dummy; } portMUX_TYPE;
#define portMUX_INITIALIZER_UNLOCKED { 0 }
extern int fake_critical_depth;
extern int fake_critical_enters;
#define portENTER_CRITICAL(m) do { (void)(m); fake_critical_depth++; fake_critical_enters++; } while (0)
#define portEXIT_CRITICAL(m) do { (void)(m); fake_critical_depth--; } while (0)
#endif
