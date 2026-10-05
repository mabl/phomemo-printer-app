//
// Phomemo Printer Application — Bluetooth SPP device backend.
//
// Registers the "btspp" URI scheme with PAPPL. The work is done in Rust
// (pm_bt_*, phomemo-pappl/src/bt): each open device keeps a PmBtConnection
// as its device data, which these callbacks hand back.
//

#include <pappl/pappl.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include "phomemo_pappl.h"

#define BT_SCHEME "btspp"

// Room for an error message from Rust.
#define BT_MESSAGE_SIZE 256

static bool bt_open_cb(pappl_device_t *device, const char *device_uri,
                       const char *name) {
    (void)name;

    char message[BT_MESSAGE_SIZE];
    PmBtConnection *connection = pm_bt_open(device_uri, message, sizeof(message));
    if (!connection) {
        papplDeviceError(device, "%s", message);
        return false;
    }

    papplDeviceSetData(device, connection);
    return true;
}

static void bt_close_cb(pappl_device_t *device) {
    pm_bt_close(papplDeviceGetData(device));
    papplDeviceSetData(device, NULL);
}

static ssize_t bt_read_cb(pappl_device_t *device, void *buffer, size_t bytes) {
    return pm_bt_read(papplDeviceGetData(device), buffer, bytes);
}

static ssize_t bt_write_cb(pappl_device_t *device, const void *buffer,
                           size_t bytes) {
    return pm_bt_write(papplDeviceGetData(device), buffer, bytes);
}

static pappl_preason_t bt_status_cb(pappl_device_t *device) {
    return (pappl_preason_t)pm_bt_status(papplDeviceGetData(device));
}

// The battery, as the one supply, once the printer has reported its level.
static int bt_supplies_cb(pappl_device_t *device, int max_supplies,
                          pappl_supply_t *supplies) {
    int level = pm_bt_battery(papplDeviceGetData(device));
    if (max_supplies < 1 || !supplies || level < 0)
        return 0;

    memset(&supplies[0], 0, sizeof(supplies[0]));
    supplies[0].color = PAPPL_SUPPLY_COLOR_NO_COLOR;
    snprintf(supplies[0].description, sizeof(supplies[0].description), "Battery");
    supplies[0].is_consumed = true;
    supplies[0].level = level;
    supplies[0].type = PAPPL_SUPPLY_TYPE_OTHER;

    return 1;
}

static char *bt_id_cb(pappl_device_t *device, char *buffer, size_t bufsize) {
    return pm_bt_device_id(papplDeviceGetData(device), buffer, bufsize) ? buffer : NULL;
}

// Register the btspp scheme with PAPPL.
void bt_add_scheme(void) {
    papplDeviceAddScheme2(BT_SCHEME, PAPPL_DEVTYPE_CUSTOM_LOCAL,
                          pm_bt_list, bt_open_cb, bt_close_cb,
                          bt_read_cb, bt_write_cb, bt_status_cb,
                          bt_supplies_cb, bt_id_cb);
}

// Whether the job prints over Bluetooth. Only a btspp device's data is a
// PmBtConnection: any other scheme's belongs to its own backend. A
// printer's device URI never changes.
static bool bt_is_job_device(pappl_job_t *job) {
    const char *uri = papplPrinterGetDeviceURI(papplJobGetPrinter(job));
    return !strncasecmp(uri, BT_SCHEME ":", sizeof(BT_SCHEME ":") - 1);
}

// Prepare to send a job, if it prints over Bluetooth: take off reports left
// over from an earlier job, so that they do not count towards this one.
void bt_start_job(pappl_job_t *job, pappl_device_t *device) {
    if (bt_is_job_device(job) && !pm_bt_discard_input(papplDeviceGetData(device)))
        papplLogJob(job, PAPPL_LOGLEVEL_WARN, "The Bluetooth connection has failed.");
}

// Wait until the printer has printed what the job sent, if it prints over
// Bluetooth: PAPPL closes the device when the job ends, which must not cut
// off data still on its way. Logs the outcome; whether every page printed,
// and true for any other device.
bool bt_wait_printed(pappl_job_t *job, pappl_device_t *device, PmJobSent sent) {
    if (sent.pages == 0 || !bt_is_job_device(job))
        return true;

    char message[BT_MESSAGE_SIZE];
    bool printed = pm_bt_wait_printed(papplDeviceGetData(device), sent.pages,
                                      sent.longest_page, message, sizeof(message));
    papplLogJob(job, printed ? PAPPL_LOGLEVEL_INFO : PAPPL_LOGLEVEL_ERROR, "%s", message);
    return printed;
}
