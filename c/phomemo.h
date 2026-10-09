//
// Phomemo Printer Application — what the C files share.
//
// The Rust library's interface is generated into phomemo_pappl.h, which this
// header includes; everything a C file defines for its own use is static.
// What the C files export is prefixed phomemo_, as the Rust library's
// exports are prefixed pm_.
//

#ifndef PHOMEMO_H
#define PHOMEMO_H

#include <stdbool.h>
#include <pappl/pappl.h>
#include "phomemo_pappl.h"

// ---------------------------------------------------------------------------
// driver.c — the printer driver
// ---------------------------------------------------------------------------

// Vendor attributes: the job options the Rust driver reads.
#define VENDOR_DITHER              "phomemo-dither"
#define VENDOR_COMPRESSION         "phomemo-compression"
#define VENDOR_OVERPRINT_VERTICAL  "phomemo-overprint-vertical"

// PAPPL's auto-add callback (pappl_pr_autoadd_cb_t): the name of the driver
// for a device, or NULL if none fits.
const char *phomemo_autoadd_cb(const char *device_info, const char *device_uri,
                               const char *device_id, void *data);

// PAPPL's driver callback (pappl_pr_driver_cb_t): fill in the capabilities
// and callbacks of driver `driver_name`; false if there is no such driver.
bool phomemo_driver_cb(pappl_system_t *system, const char *driver_name,
                       const char *device_uri, const char *device_id,
                       pappl_pr_driver_data_t *driver_data, ipp_t **driver_attrs,
                       void *data);

// Copy the printer's `<vendor option>-default` attribute, `default_name`,
// into `value`, `size` bytes; empty if it has none.
void phomemo_vendor_default(pappl_printer_t *printer, const char *default_name,
                            char *value, size_t size);

// ---------------------------------------------------------------------------
// media.c — the media setup web page
// ---------------------------------------------------------------------------

// PAPPL's printer creation callback (pappl_pr_create_cb_t): add the printer's
// media setup page.
void phomemo_media_create_cb(pappl_printer_t *printer, void *data);

// ---------------------------------------------------------------------------
// device_bt.c — the btspp device scheme
// ---------------------------------------------------------------------------

// Register the btspp scheme with PAPPL.
void phomemo_bt_add_scheme(void);

// Prepare to send a job, if it prints over Bluetooth: take off reports left
// over from an earlier job, so that they do not count towards this one.
void phomemo_bt_start_job(pappl_job_t *job, pappl_device_t *device);

// Wait until the printer has printed what the job sent, if it prints over
// Bluetooth; whether every page printed, and true for any other device.
bool phomemo_bt_wait_printed(pappl_job_t *job, pappl_device_t *device,
                             PmJobSent sent);

#endif // PHOMEMO_H
