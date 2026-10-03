/* win_taskbar_host.h: C ABI of win_taskbar_host.dll.
 *
 * Host your own native child window inside the Windows 11 x64 primary
 * horizontal taskbar. The host
 * handles attachment, the right-click menu with Move, placement persistence, DPI
 * and layout changes, taskbar auto-hide and Explorer recovery. You create and
 * draw the content window.
 *
 * Call wth_create on a thread that runs a Win32 message loop, such as a WPF
 * dispatcher or WinForms thread, and use the host only on that thread.
 * Callbacks run on that thread from inside the message loop. Keep them short.
 * Explorer shares input with that thread while the content is attached, so a
 * blocked callback stalls taskbar input.
 *
 * create_content and content_resized run with per-monitor-v2 DPI awareness, so
 * native content needs no DPI manifest. on_menu runs with the awareness of the
 * thread that called wth_create.
 *
 * Strings are UTF-8. wth_create copies the strings in options. Pointers passed
 * to callbacks are valid only during the callback.
 */
#ifndef WIN_TASKBAR_HOST_H
#define WIN_TASKBAR_HOST_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define WTH_OK 0
#define WTH_FAILED 1 /* wth_last_error says why */

#define WTH_STATE_WAITING_FOR_TASKBAR 1 /* retries on its own, e.g. while Explorer restarts */
#define WTH_STATE_ATTACHED 2
#define WTH_STATE_NO_SPACE 3            /* resumes when the width or taskbar changes */
#define WTH_STATE_FAILED 4              /* final, details go to OutputDebugString */

typedef struct wth_host wth_host;

/* Where to create the content window. */
typedef struct wth_surface {
    void *parent;   /* HWND */
    int32_t width;  /* physical pixels */
    int32_t height; /* physical pixels */
    uint32_t dpi;   /* 96 is 100% scaling */
} wth_surface;

/* Create a WS_CHILD window of surface->parent and return its HWND. The host
 * sizes, shows and later destroys it. Returning NULL fails the host. The host
 * calls this again whenever it recreates the container, so keep application
 * data outside the window. If this callback calls wth_destroy, destroy_content
 * no longer runs, so release what it created and return NULL. */
typedef void *(*wth_create_content_fn)(void *context, const wth_surface *surface);
/* The host resized `content` to the surface size, or the DPI changed. */
typedef void (*wth_content_resized_fn)(void *context, void *content, const wth_surface *surface);
/* Release your resources for `content`. A valid hosted child window is already
 * destroyed, including when wth_destroy runs inside a callback. */
typedef void (*wth_destroy_content_fn)(void *context, void *content);
/* The user chose menu_labels[index]. */
typedef void (*wth_menu_fn)(void *context, uint32_t index);
/* Show your menu at screen (x, y) in physical pixels instead of the built-in
 * one. owner is a hidden foreground window that may own a popup menu. Return
 * once the menu has closed, after wth_begin_move if the user chose to move. */
typedef void (*wth_context_menu_fn)(void *context, void *owner, int32_t x, int32_t y);

typedef struct wth_options {
    double width_dip;                /* required, 0 < width_dip <= 10000 */
    double height_dip;               /* 0 fills the taskbar height minus 4 DIP at each edge */
    const char *placement_key;       /* NULL, or keeps placement in %LOCALAPPDATA%\win-taskbar-host\<key>.placement */
    const char *const *menu_labels;  /* items above Move, & marks the access key and && shows one */
    size_t menu_count;
    void *context;                          /* passed to every callback */
    wth_create_content_fn create_content;   /* required */
    wth_content_resized_fn content_resized; /* optional */
    wth_destroy_content_fn destroy_content; /* optional */
    wth_menu_fn on_menu;                    /* required when menu_count > 0 */
    wth_context_menu_fn on_context_menu;    /* optional, replaces the menu, excludes menu items */
} wth_options;

/* Creates a host on this thread. Attachment happens later from the message loop. */
int32_t wth_create(const wth_options *options, wth_host **host);
/* Closes the host, calls destroy_content for live content and frees the handle.
 * No callback runs after it returns. Callbacks may call it, and the host then
 * finishes closing once the callback returns. */
int32_t wth_destroy(wth_host *host);
int32_t wth_get_state(wth_host *host, uint32_t *state);
/* Position along the taskbar, from 0 at the leading edge to 1 at the trailing
 * edge. 0.25 until the user moves the content or placement_key restores it. */
int32_t wth_get_position(wth_host *host, double *position);
/* Moves the content, for example from an accessible settings control, and
 * saves it under placement_key. */
int32_t wth_set_position(wth_host *host, double position);
/* Starts Move as if chosen from the built-in menu. Call it from on_context_menu
 * or a click handler, so the host may take the foreground for the drag. */
int32_t wth_begin_move(wth_host *host);
int32_t wth_set_width(wth_host *host, double width_dip);
/* This thread's last failure message, valid until its next failure. */
const char *wth_last_error(void);

#ifdef __cplusplus
}
#endif

#endif
