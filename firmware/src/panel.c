/* Stub until Task 5 adds the real OLED panel. */
#include <string.h>
#include "panel.h"

void panel_snapshot(uint8_t out[1024])
{
    memset(out, 0, 1024);
}
