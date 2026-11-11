// Nonactivating, click-through overlays share one main-thread mouse hook.
// Wheel events reach the original app unchanged unless Ctrl is resizing the frame.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <commctrl.h>
#include <magnification.h>
#include <stdbool.h>
#include <stdlib.h>

typedef struct Scanner {
    struct Scanner *next;
    HWND window, foreground;
    POINT click;
    UINT revision, actions;
    float zoom;
    bool hit;
} Scanner;

enum { FINDER_CLICK = 1u, FINDER_CONFIRM_KEY = 8u };
enum { ESC_ID = 0x5852, ENTER_ID, SPACE_ID };
static Scanner *scanners;
static HHOOK mouse_hook;
static Scanner *left_owner, *right_owner;
static bool cursor_hidden;

static Scanner *at(POINT screen, POINT *client) {
    for (Scanner *s = scanners; s; s = s->next) {
        RECT bounds;
        *client = screen;
        ScreenToClient(s->window, client);
        if (GetClientRect(s->window, &bounds) && PtInRect(&bounds, *client))
            return s;
    }
    return NULL;
}

static LRESULT CALLBACK mouse_proc(int code, WPARAM message, LPARAM data) {
    if (code == HC_ACTION) {
        MSLLHOOKSTRUCT *event = (MSLLHOOKSTRUCT *)data;
        POINT p;
        Scanner *s = at(event->pt, &p);
        if (message == WM_LBUTTONUP && left_owner) {
            left_owner->actions = (left_owner->actions & ~FINDER_CONFIRM_KEY) | FINDER_CLICK;
            left_owner = NULL;
            return 1;
        }
        if (message == WM_RBUTTONUP && right_owner) {
            right_owner->actions |= 2;
            right_owner = NULL;
            return 1;
        }
        if (s) {
            switch (message) {
            case WM_MOUSEWHEEL:
                if (GetAsyncKeyState(VK_CONTROL) & 0x8000) {
                    s->zoom += (short)HIWORD(event->mouseData) / (float)WHEEL_DELTA;
                    return 1;
                }
                // Fall through: ordinary scrolling changes the captured content.
            case WM_MOUSEHWHEEL:
                s->revision++;
                break;
            case WM_LBUTTONDOWN:
                if (s->hit) {
                    s->click = p;
                    left_owner = s;
                    return 1;
                }
                break;
            case WM_RBUTTONDOWN:
                right_owner = s;
                return 1;
            }
        }
    }
    return CallNextHookEx(NULL, code, message, data);
}

static LRESULT CALLBACK window_proc(HWND w, UINT msg, WPARAM wp, LPARAM lp, UINT_PTR id,
                                    DWORD_PTR data) {
    (void)id;
    // Normalized capture coordinates span the whole monitor, including its edges.
    // Give GPUI that same client area when SetWindowPos applies the monitor bounds.
    if (msg == WM_NCCALCSIZE) return 0;
    if (msg == WM_HOTKEY) {
        if (wp == ESC_ID) {
            ((Scanner *)data)->actions |= 2;
            return 0;
        }
        if (wp == ENTER_ID || wp == SPACE_ID) {
            POINT cursor, p;
            GetCursorPos(&cursor);
            Scanner *s = at(cursor, &p);
            if (s) {
                s->click = p;
                s->actions = (s->actions & ~FINDER_CLICK) | FINDER_CONFIRM_KEY;
            }
            return 0;
        }
    }
    if (msg == WM_MOUSEACTIVATE) return MA_NOACTIVATE;
    if (msg == WM_NCHITTEST) return HTTRANSPARENT;
    return DefSubclassProc(w, msg, wp, lp);
}

void *xrose_win_begin(HWND w, HMONITOR monitor) {
    Scanner *s = calloc(1, sizeof(*s));
    if (!s) return NULL;
    s->window = w;
    s->foreground = GetForegroundWindow();
    if (!SetWindowSubclass(w, window_proc, ESC_ID, (DWORD_PTR)s)) {
        free(s);
        return NULL;
    }
    if (!scanners) {
        mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, GetModuleHandleW(NULL), 0);
        if (!mouse_hook) {
            RemoveWindowSubclass(w, window_proc, ESC_ID);
            free(s);
            return NULL;
        }
        RegisterHotKey(w, ESC_ID, MOD_NOREPEAT, VK_ESCAPE);
        RegisterHotKey(w, ENTER_ID, MOD_NOREPEAT, VK_RETURN);
        RegisterHotKey(w, SPACE_ID, MOD_NOREPEAT, VK_SPACE);
        // The underlying application owns the pointer over a click-through window.
        // Hide the system cursor once for all overlays sharing this hook.
        if (MagInitialize()) {
            cursor_hidden = MagShowSystemCursor(FALSE) != FALSE;
            if (!cursor_hidden) MagUninitialize();
        }
    }
    s->next = scanners;
    scanners = s;
    SetWindowLongPtrW(w, GWL_EXSTYLE, GetWindowLongPtrW(w, GWL_EXSTYLE)
                                      | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT);
    SetLayeredWindowAttributes(w, 0, 255, LWA_ALPHA);
    MONITORINFO info = {0};
    info.cbSize = sizeof(info);
    if (GetMonitorInfoW(monitor, &info)) {
        RECT r = info.rcMonitor;
        SetWindowPos(w, HWND_TOPMOST, r.left, r.top, r.right-r.left, r.bottom-r.top,
                     SWP_NOACTIVATE | SWP_FRAMECHANGED);
    } else {
        SetWindowPos(w, HWND_TOPMOST, 0, 0, 0, 0,
                     SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE);
    }
    return s;
}

bool xrose_win_import_sound(bool success) {
    return MessageBeep(success ? MB_OK : MB_ICONERROR) != 0;
}

void xrose_win_set_hit(Scanner *s, bool hit) {
    s->hit = hit;
}

void xrose_win_capture(Scanner *s, bool exclude) {
    // Exclude only during capture: permanent exclusion also hides the finder
    // on remote / virtual displays. All HWND changes stay on the UI thread.
    SetWindowDisplayAffinity(s->window, exclude ? 0x11 : WDA_NONE);
}

UINT xrose_win_input(Scanner *s, float *x, float *y, float *cx, float *cy, UINT *rev, float *zoom) {
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
    *zoom = s->zoom;
    s->zoom = 0;
    UINT actions = s->actions;
    s->actions = 0;
    if (GetAsyncKeyState(VK_LBUTTON) & 0x8000) actions |= 4;
    if (GetAsyncKeyState(VK_ESCAPE) & 0x8000) actions |= 2;
    return actions;
}

void xrose_win_end(Scanner *s) {
    UnregisterHotKey(s->window, ESC_ID);
    UnregisterHotKey(s->window, ENTER_ID);
    UnregisterHotKey(s->window, SPACE_ID);
    RemoveWindowSubclass(s->window, window_proc, ESC_ID);
    for (Scanner **p = &scanners; *p; p = &(*p)->next) {
        if (*p == s) {
            *p = s->next;
            break;
        }
    }
    if (!scanners) {
        UnhookWindowsHookEx(mouse_hook);
        mouse_hook = NULL;
        if (cursor_hidden) {
            MagShowSystemCursor(TRUE);
            MagUninitialize();
            cursor_hidden = false;
        }
    }
    if (left_owner == s) left_owner = NULL;
    if (right_owner == s) right_owner = NULL;
    free(s);
}
