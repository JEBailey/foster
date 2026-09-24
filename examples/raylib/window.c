#include "raylib.h"
#include "window.h"

void DemoInitWindow(int width, int height) {
    static const char title[] = "Foster + raylib | C struct integration";
    InitWindow(width, height, title);
}
