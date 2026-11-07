// A nonactivating topmost window owns the scanner's cursor and buttons. Wheel
// messages retain their original coordinates, modifiers and high-resolution delta.
#define WIN32_LEAN_AND_MEAN
#include <stdbool.h>
#include <stdlib.h>
// Common controls depend on the Win32 types declared by windows.h.
#include <windows.h>
#include <commctrl.h>
#include <windowsx.h>

typedef struct {
    HWND window, foreground;
    POINT click;
    UINT revision, actions;
    BOOL hotkey, cursorVisible;
} Scanner;

#define ESC_ID 0x5852

void xrose_win_cursor(Scanner *s, bool visible) {
    s->cursorVisible = visible;
    SetCursor(visible ? LoadCursorW(NULL, MAKEINTRESOURCEW(32512)) : NULL);
}

static HWND beneath(HWND overlay, POINT p) {
    for (HWND w = GetWindow(overlay, GW_HWNDNEXT); w; w = GetWindow(w, GW_HWNDNEXT)) {
        RECT bounds;
        if (!IsWindowVisible(w) || IsIconic(w) || !GetWindowRect(w, &bounds) || !PtInRect(&bounds, p))
            continue;
        for (;;) {
            POINT client = p;
            ScreenToClient(w, &client);
            HWND child = ChildWindowFromPointEx(
                w, client, CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT);
            if (!child || child == w)
                return w;
            w = child;
        }
    }
    return NULL;
}

static LRESULT CALLBACK window_proc(HWND w, UINT msg, WPARAM wp, LPARAM lp, UINT_PTR id,
                                    DWORD_PTR data) {
    (void)id;
    Scanner *s = (Scanner *)data;
    switch (msg) {
    case WM_HOTKEY:
        if (wp != ESC_ID)
            break;
        s->actions |= 2;
        return 0;
    case WM_MOUSEACTIVATE:
        return MA_NOACTIVATE;
    case WM_SETCURSOR:
        xrose_win_cursor(s, s->cursorVisible);
        return TRUE;
    case WM_LBUTTONDOWN:
        s->click.x = GET_X_LPARAM(lp);
        s->click.y = GET_Y_LPARAM(lp);
        s->actions |= 1;
        return 0;
    case WM_RBUTTONDOWN:
        s->actions |= 2;
        return 0;
    case WM_LBUTTONUP:
    case WM_RBUTTONUP:
        return 0;
    case WM_MOUSEWHEEL:
    case WM_MOUSEHWHEEL: {
        POINT p = {GET_X_LPARAM(lp), GET_Y_LPARAM(lp)};
        HWND target = beneath(w, p);
        if (target)
            PostMessageW(target, msg, wp, lp);
        s->revision++;
        return 0;
    }
    }
    return DefSubclassProc(w, msg, wp, lp);
}
void *xrose_win_begin(HWND w) {
    Scanner *s = calloc(1, sizeof(*s));
    if (!s)
        return NULL;
    s->window = w;
    if (!SetWindowSubclass(w, window_proc, ESC_ID, (DWORD_PTR)s)) {
        free(s);
        return NULL;
    }
    s->hotkey = RegisterHotKey(w, ESC_ID, MOD_NOREPEAT, VK_ESCAPE);
    SetWindowLongPtrW(w, GWL_EXSTYLE, GetWindowLongPtrW(w, GWL_EXSTYLE) | WS_EX_NOACTIVATE);
    SetWindowDisplayAffinity(w, 0x11 /* WDA_EXCLUDEFROMCAPTURE */);
    SetWindowPos(w, HWND_TOPMOST, 0, 0, 0, 0,
                 SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    SetCursor(NULL);
    return s;
}
UINT xrose_win_input(Scanner *s, float *x, float *y, float *cx, float *cy, UINT *rev) {
    POINT p;
    GetCursorPos(&p);
    ScreenToClient(s->window, &p);
    *x = (float)p.x;
    *y = (float)p.y;
    *cx = (float)s->click.x;
    *cy = (float)s->click.y;
    HWND front = GetForegroundWindow();
    if (front != s->foreground) {
        s->foreground = front;
        s->revision++;
    }
    *rev = s->revision;
    UINT actions = s->actions;
    s->actions = 0;
    if (!s->hotkey && (GetAsyncKeyState(VK_ESCAPE) & 0x8000))
        actions |= 2;
    return actions;
}
void xrose_win_end(Scanner *s) {
    if (s->hotkey)
        UnregisterHotKey(s->window, ESC_ID);
    RemoveWindowSubclass(s->window, window_proc, ESC_ID);
    SetCursor(LoadCursorW(NULL, MAKEINTRESOURCEW(32512)));
    free(s);
}
