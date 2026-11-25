#include <linux/input-event-codes.h>
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
    if (strcmp(interface, org_kde_kwin_fake_input_interface.name) != 0 ||
        version < ORG_KDE_KWIN_FAKE_INPUT_DESTROY_SINCE_VERSION)
        return;
    uint32_t supported = (uint32_t)org_kde_kwin_fake_input_interface.version;
    *input = wl_registry_bind(registry, name, &org_kde_kwin_fake_input_interface,
                              version < supported ? version : supported);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener listener = {global, global_remove};

static int send_keycode(struct org_kde_kwin_fake_input *input, struct xkb_keymap *keymap,
                        xkb_keysym_t sym)
{
    for (xkb_keycode_t key = xkb_keymap_min_keycode(keymap);
         key <= xkb_keymap_max_keycode(keymap); ++key) {
        for (xkb_level_index_t level = 0; level < 2; ++level) {
            const xkb_keysym_t *syms;
            if (xkb_keymap_key_get_syms_by_level(keymap, key, 0, level, &syms) != 1 ||
                syms[0] != sym)
                continue;
            if (level)
                org_kde_kwin_fake_input_keyboard_key(input, KEY_LEFTSHIFT, WL_KEYBOARD_KEY_STATE_PRESSED);
            // XKB's evdev keymap adds 8 to Linux input keycodes.
            org_kde_kwin_fake_input_keyboard_key(input, key - 8, WL_KEYBOARD_KEY_STATE_PRESSED);
            org_kde_kwin_fake_input_keyboard_key(input, key - 8, WL_KEYBOARD_KEY_STATE_RELEASED);
            if (level)
                org_kde_kwin_fake_input_keyboard_key(input, KEY_LEFTSHIFT, WL_KEYBOARD_KEY_STATE_RELEASED);
            return 0;
        }
    }
    fputs("nested input: character is not on the v5 US keymap\n", stderr);
    return 1;
}

static int send_text(struct org_kde_kwin_fake_input *input, struct xkb_keymap *keymap,
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
#ifdef ORG_KDE_KWIN_FAKE_INPUT_KEYBOARD_KEYSYM_SINCE_VERSION
        if (!keymap) {
            org_kde_kwin_fake_input_keyboard_keysym(input, sym, WL_KEYBOARD_KEY_STATE_PRESSED);
            org_kde_kwin_fake_input_keyboard_keysym(input, sym, WL_KEYBOARD_KEY_STATE_RELEASED);
        } else
#endif
        if (send_keycode(input, keymap, sym))
            return 1;
        text += n;
        remaining -= n;
    }
    return 0;
}

static const struct {
    const char *name;
    uint32_t code;
} key_names[] = {
    {"ctrl", KEY_LEFTCTRL}, {"shift", KEY_LEFTSHIFT}, {"alt", KEY_LEFTALT},
    {"super", KEY_LEFTMETA}, {"left", KEY_LEFT}, {"right", KEY_RIGHT}, {"up", KEY_UP},
    {"down", KEY_DOWN}, {"enter", KEY_ENTER}, {"tab", KEY_TAB}, {"esc", KEY_ESC},
    {"a", KEY_A}, {"b", KEY_B}, {"c", KEY_C}, {"d", KEY_D}, {"e", KEY_E}, {"f", KEY_F},
    {"g", KEY_G}, {"h", KEY_H}, {"i", KEY_I}, {"j", KEY_J}, {"k", KEY_K}, {"l", KEY_L},
    {"m", KEY_M}, {"n", KEY_N}, {"o", KEY_O}, {"p", KEY_P}, {"q", KEY_Q}, {"r", KEY_R},
    {"s", KEY_S}, {"t", KEY_T}, {"u", KEY_U}, {"v", KEY_V}, {"w", KEY_W}, {"x", KEY_X},
    {"y", KEY_Y}, {"z", KEY_Z},
};

static int key_code(const char *name, size_t length, uint32_t *code)
{
    for (size_t i = 0; i < sizeof key_names / sizeof key_names[0]; ++i) {
        if (strlen(key_names[i].name) == length && strncmp(key_names[i].name, name, length) == 0) {
            *code = key_names[i].code;
            return 0;
        }
    }
    fprintf(stderr, "nested input: unknown key %.*s\n", (int)length, name);
    return 1;
}

// Chords such as "ctrl+shift+e,ctrl+alt+left": each name is pressed in order, then
// released in reverse. Keycodes follow the US keymap on every fake input version, so
// modifier state does not depend on how KWin maps a keysym.
static int send_chords(struct wl_display *display, struct org_kde_kwin_fake_input *input,
                       const char *chords)
{
    uint32_t held[8];
    const char *name = chords;
    while (*name) {
        size_t count = 0;
        for (;;) {
            size_t length = strcspn(name, "+,");
            if (!length || count == sizeof held / sizeof held[0] ||
                key_code(name, length, &held[count]))
                return 1;
            org_kde_kwin_fake_input_keyboard_key(input, held[count++], WL_KEYBOARD_KEY_STATE_PRESSED);
            name += length;
            if (*name != '+')
                break;
            ++name;
        }
        while (count)
            org_kde_kwin_fake_input_keyboard_key(input, held[--count], WL_KEYBOARD_KEY_STATE_RELEASED);
        if (*name == ',')
            ++name;
    }
    if (wl_display_roundtrip(display) < 0) {
        perror("nested input: send keys");
        return 1;
    }
    return 0;
}

static int type_text(struct wl_display *display, struct org_kde_kwin_fake_input *input,
                     const char *text)
{
    struct xkb_keymap *keymap = NULL;
#ifdef ORG_KDE_KWIN_FAKE_INPUT_KEYBOARD_KEYSYM_SINCE_VERSION
    if (org_kde_kwin_fake_input_get_version(input) < ORG_KDE_KWIN_FAKE_INPUT_KEYBOARD_KEYSYM_SINCE_VERSION)
#endif
    {
        const struct xkb_rule_names names = {"evdev", "pc105", "us", "", ""};
        struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
        if (context) {
            keymap = xkb_keymap_new_from_names(context, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
            xkb_context_unref(context);
        }
        if (!keymap) {
            fputs("nested input: could not load the US keymap\n", stderr);
            return 1;
        }
    }
    int status = send_text(input, keymap, text);
    xkb_keymap_unref(keymap);
    if (status == 0 && wl_display_roundtrip(display) < 0) {
        perror("nested input: send keys");
        return 1;
    }
    return status;
}

int main(int argc, char **argv)
{
    int chords = argc == 3 && strcmp(argv[1], "--keys") == 0;
    if ((argc != 2 && !chords) || !setlocale(LC_CTYPE, "C.UTF-8")) {
        fputs("nested input: expected TEXT or --keys CHORDS, and a C.UTF-8 locale\n", stderr);
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
        fputs("nested input: KWin fake input v5 or newer is not available\n", stderr);
    } else {
        org_kde_kwin_fake_input_authenticate(input, "sumie nested test",
                                            "Type into the focused test client");
        status = chords ? send_chords(display, input, argv[2]) : type_text(display, input, argv[1]);
        org_kde_kwin_fake_input_destroy(input);
    }
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return status;
}
