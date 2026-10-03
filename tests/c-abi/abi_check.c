/* Checks x64 layout, argument validation and reentrant destruction against
 * the real taskbar. Run through run.ps1. */
#include <stdio.h>
#include <windows.h>

#include "win_taskbar_host.h"

_Static_assert(sizeof(wth_surface) == 24, "wth_surface matches the DLL on x64");
_Static_assert(sizeof(wth_options) == 88, "wth_options matches the DLL on x64");

static int failures;

static void check(int passed, const char *what)
{
    printf("%s - %s\n", passed ? "ok" : "FAILED", what);
    failures += !passed;
}

static void *no_content(void *context, const wth_surface *surface)
{
    (void)context;
    (void)surface;
    return NULL;
}

static void no_menu(void *context, uint32_t index)
{
    (void)context;
    (void)index;
}

/* Expects wth_create to reject `options`, clear the output and explain why. */
static void expect_invalid(const wth_options *options, const char *what)
{
    wth_host *host = (wth_host *)(uintptr_t)1;
    int32_t status = wth_create(options, &host);
    printf("   %s: \"%s\"\n", what, wth_last_error());
    check(status == WTH_FAILED && host == NULL && wth_last_error()[0] != '\0', what);
    if (status == WTH_OK && host != NULL) wth_destroy(host);
}

typedef struct reentrant_destroy {
    wth_host *host;
    int created;
    int resized;
    int destroyed;
    int destroyed_in_call;
    int window_gone_in_destroy;
    int32_t resized_status;
    int32_t nested_status;
} reentrant_destroy;

static void *create_window(void *context, const wth_surface *surface)
{
    reentrant_destroy *test = context;
    HWND child = CreateWindowExW(0, L"STATIC", L"ABI check", WS_CHILD | WS_VISIBLE,
        0, 0, surface->width, surface->height, surface->parent, NULL, NULL, NULL);
    test->created = child != NULL;
    return child;
}

static void destroy_window(void *context, void *content)
{
    reentrant_destroy *test = context;
    test->window_gone_in_destroy = !IsWindow(content);
    test->destroyed++;
    test->nested_status = wth_destroy(test->host);
}

/* Closes the host from inside a callback, as an Exit menu item would. */
static void resize_destroys(void *context, void *content, const wth_surface *surface)
{
    reentrant_destroy *test = context;
    (void)content;
    (void)surface;
    test->resized_status = wth_destroy(test->host);
    test->destroyed_in_call = test->destroyed;
    test->host = NULL;
    test->resized = 1;
}

/* Runs the message loop until *done or five seconds pass. */
static void pump(const int *done)
{
    ULONGLONG deadline = GetTickCount64() + 5000;
    while (!*done && GetTickCount64() < deadline) {
        MSG message;
        while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Sleep(10);
    }
}

static void check_reentrant_destroy(void)
{
    reentrant_destroy test = {0};
    wth_options options = {0};
    options.width_dip = 48;
    options.context = &test;
    options.create_content = create_window;
    options.content_resized = resize_destroys;
    options.destroy_content = destroy_window;
    test.resized_status = -1;
    check(wth_create(&options, &test.host) == WTH_OK, "desktop host created");
    pump(&test.created);
    check(test.created, "desktop content created");
    if (test.host) check(wth_set_width(test.host, 60) == WTH_OK, "width change accepted");
    pump(&test.resized);
    check(test.resized_status == WTH_OK, "content_resized destroys the host");
    if (test.host) wth_destroy(test.host);
    check(test.destroyed_in_call == 1 && test.destroyed == 1, "destroy_content runs once, inside that wth_destroy");
    check(test.window_gone_in_destroy, "the child window is destroyed before consumer resources are released");
    check(test.nested_status == WTH_FAILED, "reentrant destruction is rejected");
}

int main(void)
{
    static const char *const labels[] = {"&Exit"};
    wth_options options = {0};

    expect_invalid(NULL, "wth_create(NULL options)");
    options.width_dip = 100;
    expect_invalid(&options, "wth_create(no create_content)");
    options.create_content = no_content;
    options.menu_count = 1;
    options.menu_labels = labels;
    expect_invalid(&options, "wth_create(menu labels without on_menu)");
    options.menu_labels = NULL;
    options.on_menu = no_menu;
    expect_invalid(&options, "wth_create(menu_count without menu_labels)");
    options.menu_count = 0;
    check(wth_create(&options, NULL) == WTH_FAILED, "wth_create(NULL host output)");
    check(wth_destroy(NULL) == WTH_FAILED, "wth_destroy(NULL)");
    check(wth_set_width(NULL, 10) == WTH_FAILED, "wth_set_width(NULL)");
    check(wth_begin_move(NULL) == WTH_FAILED, "wth_begin_move(NULL)");

    check_reentrant_destroy();

    if (failures) {
        printf("%d check(s) FAILED\n", failures);
        return 1;
    }
    printf("all checks passed\n");
    return 0;
}
