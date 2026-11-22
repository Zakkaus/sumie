#include <locale.h>
#include <stdio.h>
#include <string.h>
#include <uchar.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>

#include "fake-input-client.h"

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version)
{
    struct org_kde_kwin_fake_input **input = data;
    if (strcmp(interface, org_kde_kwin_fake_input_interface.name) == 0 &&
        version >= ORG_KDE_KWIN_FAKE_INPUT_KEYBOARD_KEYSYM_SINCE_VERSION) {
        *input = wl_registry_bind(registry, name, &org_kde_kwin_fake_input_interface,
                                  ORG_KDE_KWIN_FAKE_INPUT_KEYBOARD_KEYSYM_SINCE_VERSION);
    }
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener listener = {global, global_remove};

static int send_text(struct wl_display *display, struct org_kde_kwin_fake_input *input,
                     const char *text)
{
    mbstate_t state = {0};
    size_t remaining = strlen(text);
    while (remaining) {
        char32_t codepoint;
        size_t n = mbrtoc32(&codepoint, text, remaining, &state);
        if (n > remaining || (codepoint < 32 && codepoint != '\n' && codepoint != '\t') ||
            codepoint == 127) {
            fputs("nested input: expected UTF-8 text with optional newlines and tabs\n", stderr);
            return 1;
        }
        uint32_t sym = codepoint == '\n' ? XKB_KEY_Return :
                       codepoint == '\t' ? XKB_KEY_Tab : xkb_utf32_to_keysym(codepoint);
        org_kde_kwin_fake_input_keyboard_keysym(input, sym, WL_KEYBOARD_KEY_STATE_PRESSED);
        org_kde_kwin_fake_input_keyboard_keysym(input, sym, WL_KEYBOARD_KEY_STATE_RELEASED);
        text += n;
        remaining -= n;
    }
    if (wl_display_roundtrip(display) < 0) {
        perror("nested input: send keys");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (argc != 2 || !setlocale(LC_CTYPE, "C.UTF-8")) {
        fputs("nested input: expected one text argument and a C.UTF-8 locale\n", stderr);
        return 2;
    }
    struct wl_display *display = wl_display_connect("wl-sumie");
    if (!display) {
        perror("nested input: connect to wl-sumie");
        return 1;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    struct org_kde_kwin_fake_input *input = NULL;
    wl_registry_add_listener(registry, &listener, &input);
    int status = 1;
    if (wl_display_roundtrip(display) < 0) {
        perror("nested input: read globals");
    } else if (!input) {
        fputs("nested input: KWin fake input v6 is not available\n", stderr);
    } else {
        org_kde_kwin_fake_input_authenticate(input, "sumie nested test",
                                            "Type into the focused test client");
        status = send_text(display, input, argv[1]);
        org_kde_kwin_fake_input_destroy(input);
    }
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return status;
}
