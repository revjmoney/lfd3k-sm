#pragma once

struct android_app;

void ui_init_style(android_app *app, float scale);
void ui_set_metrics(float dpi, float font_scale, float left, float top, float right, float bottom);
float ui_style_scale();
float ui_font_px();
void ui_apply_style_scale();
void ui_frame(android_app *app);
void ui_shutdown(android_app *app);
/* Host layout check. screen: 0 connect, 1 calls, 2 directory, 3 settings.
 * Returns horizontal overflow in pixels (0 when the content fits). */
float ui_preview(int screen);
