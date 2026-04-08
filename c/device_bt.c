//
// Phomemo Printer Application — Bluetooth SPP device backend.
//
// Registers the "btspp" URI scheme with PAPPL.  Delegates to Rust
// FFI functions (pm_bt_*) for discovery, RFCOMM I/O, and status.
//

#include <pappl/pappl.h>
#include <stdio.h>
#include <string.h>
#include "phomemo_pappl.h"

// The generated header already declares pm_bt_* functions with the
// correct types (BtConnectionHandle*, etc.), so no extern declarations
// needed here — just use them directly.

// ---------------------------------------------------------------------------
// list_cb — enumerate paired BT devices matching Phomemo printers
// ---------------------------------------------------------------------------

bool bt_list_cb(pappl_device_cb_t cb, void *data,
                pappl_deverror_cb_t err_cb, void *err_data) {
    bool found = pm_bt_list(cb, data, NULL, NULL);
    if (!found && err_cb)
        err_cb("No Phomemo Bluetooth devices found (BlueZ discovery may have timed out).", err_data);
    return found;
}

// ---------------------------------------------------------------------------
// open_cb — connect to a btspp:// URI
// ---------------------------------------------------------------------------

bool bt_open_cb(pappl_device_t *device, const char *device_uri,
                const char *name) {
    (void)name;

    struct BtConnectionHandle *handle = pm_bt_open(device_uri, 5000);
    if (!handle) {
        papplDeviceError(device, "Failed to connect to %s", device_uri);
        return false;
    }

    papplDeviceSetData(device, handle);
    return true;
}

// ---------------------------------------------------------------------------
// close_cb
// ---------------------------------------------------------------------------

void bt_close_cb(pappl_device_t *device) {
    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    if (handle) {
        pm_bt_close(handle);
        papplDeviceSetData(device, NULL);
    }
}

// ---------------------------------------------------------------------------
// read_cb — no timeout param; SO_RCVTIMEO set during open
// ---------------------------------------------------------------------------

ssize_t bt_read_cb(pappl_device_t *device, void *buffer, size_t bytes) {
    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    if (!handle)
        return -1;
    return (ssize_t)pm_bt_read(handle, (uint8_t *)buffer, bytes);
}

// ---------------------------------------------------------------------------
// write_cb — delegates to Rust 1024-byte chunked writer
// ---------------------------------------------------------------------------

ssize_t bt_write_cb(pappl_device_t *device, const void *buffer, size_t bytes) {
    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    if (!handle)
        return -1;
    return (ssize_t)pm_bt_write(handle, (const uint8_t *)buffer, bytes);
}

// ---------------------------------------------------------------------------
// status_cb — query paper/cover/temperature via Rust
// ---------------------------------------------------------------------------

pappl_preason_t bt_status_cb(pappl_device_t *device) {
    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    if (!handle)
        return PAPPL_PREASON_OFFLINE;
    return (pappl_preason_t)pm_bt_status(handle);
}

// ---------------------------------------------------------------------------
// supplies_cb — report battery level
// ---------------------------------------------------------------------------

int bt_supplies_cb(pappl_device_t *device, int max_supplies,
                   pappl_supply_t *supplies) {
    if (max_supplies < 1 || !supplies)
        return 0;

    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    int battery = handle ? pm_bt_battery(handle) : -1;

    memset(&supplies[0], 0, sizeof(supplies[0]));
    supplies[0].color = PAPPL_SUPPLY_COLOR_NO_COLOR;
    snprintf(supplies[0].description, sizeof(supplies[0].description), "Battery");
    supplies[0].is_consumed = true;
    supplies[0].level = battery;  // 0-100 or -1 (unknown)
    supplies[0].type = PAPPL_SUPPLY_TYPE_OTHER;

    return 1;
}

// ---------------------------------------------------------------------------
// id_cb — return IEEE 1284 device ID string
// ---------------------------------------------------------------------------

char *bt_id_cb(pappl_device_t *device, char *buffer, size_t bufsize) {
    struct BtConnectionHandle *handle = papplDeviceGetData(device);
    const char *model = (handle) ? pm_bt_model_name(handle) : "";
    if (model[0])
        snprintf(buffer, bufsize, "MFG:Phomemo;MDL:%s;CMD:PHOMEMO;", model);
    else
        snprintf(buffer, bufsize, "MFG:Phomemo;CMD:PHOMEMO;");
    return buffer;
}
