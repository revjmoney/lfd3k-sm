#pragma once

struct android_app;

void ui_init_style(android_app *app, float scale);
void ui_frame(android_app *app);
void ui_shutdown(android_app *app);
