#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>

#include "ext-data-control-client.h"

static const char mime[] = "text/plain;charset=utf-8";

struct fixture {
    struct wl_seat *seat;
    struct ext_data_control_manager_v1 *manager;
    struct ext_data_control_offer_v1 *offer[2];
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version)
{
    struct fixture *fixture = data;
    (void)version;
    if (!fixture->seat && strcmp(interface, wl_seat_interface.name) == 0)
        fixture->seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
    if (strcmp(interface, ext_data_control_manager_v1_interface.name) == 0)
        fixture->manager = wl_registry_bind(registry, name, &ext_data_control_manager_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {global, global_remove};

static void offered(void *data, struct ext_data_control_offer_v1 *offer, const char *type)
{
    (void)data;
    if (strcmp(type, mime) == 0)
        ext_data_control_offer_v1_set_user_data(offer, offer);
}

static const struct ext_data_control_offer_v1_listener offer_listener = {offered};

static void data_offer(void *data, struct ext_data_control_device_v1 *device,
                       struct ext_data_control_offer_v1 *offer)
{
    (void)data;
    (void)device;
    ext_data_control_offer_v1_add_listener(offer, &offer_listener, NULL);
}

static void set_offer(struct ext_data_control_offer_v1 **slot, struct ext_data_control_offer_v1 *offer)
{
    if (*slot)
        ext_data_control_offer_v1_destroy(*slot);
    *slot = offer;
}

static void selection(void *data, struct ext_data_control_device_v1 *device,
                      struct ext_data_control_offer_v1 *offer)
{
    (void)device;
    set_offer(&((struct fixture *)data)->offer[0], offer);
}

static void primary_selection(void *data, struct ext_data_control_device_v1 *device,
                              struct ext_data_control_offer_v1 *offer)
{
    (void)device;
    set_offer(&((struct fixture *)data)->offer[1], offer);
}

static void finished(void *data, struct ext_data_control_device_v1 *device)
{
    (void)data;
    (void)device;
}

static const struct ext_data_control_device_v1_listener device_listener = {
    .data_offer = data_offer, .selection = selection, .primary_selection = primary_selection,
    .finished = finished,
};

static int receive(struct wl_display *display, struct ext_data_control_offer_v1 *offer)
{
    if (!offer || !ext_data_control_offer_v1_get_user_data(offer))
        return 3;
    int fds[2];
    if (pipe(fds) < 0)
        return 2;
    ext_data_control_offer_v1_receive(offer, mime, fds[1]);
    close(fds[1]);
    int status = 2;
    if (wl_display_roundtrip(display) >= 0) {
        char chunk[4096];
        ssize_t count;
        while ((count = read(fds[0], chunk, sizeof chunk)) > 0) {
            if (fwrite(chunk, 1, (size_t)count, stdout) != (size_t)count)
                break;
        }
        if (count == 0)
            status = 0;
    }
    close(fds[0]);
    return status;
}

int main(int argc, char **argv)
{
    if (argc != 2 || (strcmp(argv[1], "primary") != 0 && strcmp(argv[1], "clipboard") != 0))
        return 2;
    struct wl_display *display = wl_display_connect("wl-sumie");
    if (!display) {
        perror("selection fixture: connect to wl-sumie");
        return 2;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    struct fixture fixture = {0};
    wl_registry_add_listener(registry, &registry_listener, &fixture);
    if (wl_display_roundtrip(display) < 0 || !fixture.seat || !fixture.manager) {
        fputs("selection fixture: no ext-data-control-v1 or seat\n", stderr);
        wl_display_disconnect(display);
        return 2;
    }
    struct ext_data_control_device_v1 *device =
        ext_data_control_manager_v1_get_data_device(fixture.manager, fixture.seat);
    ext_data_control_device_v1_add_listener(device, &device_listener, &fixture);
    int primary = strcmp(argv[1], "primary") == 0;
    int status = wl_display_roundtrip(display) < 0 ? 2 : receive(display, fixture.offer[primary]);
    wl_display_disconnect(display);
    return status;
}
