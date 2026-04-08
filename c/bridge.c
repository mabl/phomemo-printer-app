//
// Phomemo Printer Application — PAPPL accessor bridge.
//
// Thin wrapper functions that extract fields from PAPPL opaque types
// and expose them to Rust via the PhomemoOps vtable.
//

#include <pappl/pappl.h>
#include "phomemo_pappl.h"

ssize_t bridge_write(void *device, const unsigned char *data, size_t len) {
    return papplDeviceWrite((pappl_device_t *)device, data, len);
}

ssize_t bridge_read(void *device, unsigned char *buf, size_t len) {
    return papplDeviceRead((pappl_device_t *)device, buf, len);
}

void bridge_flush(void *device) {
    papplDeviceFlush((pappl_device_t *)device);
}

void bridge_log(void *job, int level, const char *msg) {
    papplLogJob((pappl_job_t *)job, (pappl_loglevel_t)level, "%s", msg);
}

bool bridge_is_canceled(void *job) {
    return papplJobIsCanceled((pappl_job_t *)job);
}

unsigned bridge_bytes_per_line(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->header.cupsBytesPerLine;
}

unsigned bridge_width_px(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->header.cupsWidth;
}

unsigned bridge_height_px(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->header.cupsHeight;
}

int bridge_get_print_darkness(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->print_darkness;
}

int bridge_get_print_speed(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->print_speed;
}

unsigned bridge_get_media_tracking(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    unsigned tracking = (unsigned)o->media.tracking;

    // Web UI can keep "Gap" selected when a synthetic 0mm-length
    // continuous roll size is chosen. Treat 0-length media as continuous
    // unless the user explicitly selected black mark tracking.
    if (o->media.size_length == 0 && tracking != PAPPL_MEDIA_TRACKING_MARK)
        tracking = PAPPL_MEDIA_TRACKING_CONTINUOUS;

    return tracking;
}

int bridge_get_orientation(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return (int)o->orientation_requested;
}

unsigned bridge_get_copies(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->copies;
}

unsigned bridge_get_bits_per_pixel(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return o->header.cupsBitsPerPixel;
}

unsigned bridge_get_color_space(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return (unsigned)o->header.cupsColorSpace;
}

unsigned bridge_get_content_optimize(const void *options) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return (unsigned)o->print_content_optimize;
}

// Read vendor option by name from job options.
// Returns NULL if not found or not set.
const char *bridge_get_vendor_option(const void *options, const char *name) {
    const pappl_pr_options_t *o = (const pappl_pr_options_t *)options;
    return cupsGetOption(name, o->num_vendor, o->vendor);
}

bool bridge_model_supports_compression(void *job) {
    pappl_printer_t *printer = papplJobGetPrinter((pappl_job_t *)job);
    if (!printer)
        return true;

    pappl_pr_driver_data_t dd;
    if (!papplPrinterGetDriverData(printer, &dd) || !dd.extension)
        return true;

    const struct ModelInfoC *model = (const struct ModelInfoC *)dd.extension;
    return model->supports_compression;
}
