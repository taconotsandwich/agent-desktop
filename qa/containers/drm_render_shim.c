// QA harness shim: KWin's virtual backend requires a DRM render node, but the
// only software DRM device GitHub-hosted runners can offer (vkms) exposes just
// a primary node, so the compositor falls back to QPainter and every
// ScreenShot2 capture is cancelled. Present a primary node as a render node to
// kwin_wayland so compositing initializes EGL on that device.
//
// Scoped to kwin_wayland and to devices that have no render node: inert on
// hosts with real render nodes and for every other process in the session.
#define _GNU_SOURCE

#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <xf86drm.h>

typedef int (*DrmGetDevices2)(unsigned int, drmDevicePtr[], int);

static int kwin_wayland_process(void) {
    char name[64];
    FILE *file = fopen("/proc/self/comm", "r");
    if (!file) {
        return 0;
    }
    const char *read = fgets(name, sizeof(name), file);
    fclose(file);
    if (!read) {
        return 0;
    }
    name[strcspn(name, "\n")] = '\0';
    return strcmp(name, "kwin_wayland") == 0;
}

int drmGetDevices2(unsigned int flags, drmDevicePtr devices[], int max_devices) {
    static DrmGetDevices2 real;
    if (!real) {
        real = (DrmGetDevices2)dlsym(RTLD_NEXT, "drmGetDevices2");
        if (!real) {
            return -1;
        }
    }
    const int count = real(flags, devices, max_devices);
    if (count <= 0 || !devices || !kwin_wayland_process()) {
        return count;
    }
    const int primary = 1 << DRM_NODE_PRIMARY;
    const int render = 1 << DRM_NODE_RENDER;
    for (int index = 0; index < count; index++) {
        drmDevicePtr device = devices[index];
        if (!device) {
            continue;
        }
        if ((device->available_nodes & primary) && !(device->available_nodes & render)) {
            device->available_nodes |= render;
            device->nodes[DRM_NODE_RENDER] = device->nodes[DRM_NODE_PRIMARY];
        }
    }
    return count;
}
