//
// Phomemo Printer Application — driver callbacks.
//
// tp_driver_cb fills pappl_pr_driver_data_t with model-specific capabilities.
// Raster callbacks bridge into Rust via the PhomemoOps vtable.
//

#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <stdint.h>
#include <pappl/pappl.h>
#include "phomemo_pappl.h"

// ---------------------------------------------------------------------------
// Bridge functions — thin wrappers giving Rust access to PAPPL types
// (defined in bridge.c)
// ---------------------------------------------------------------------------
extern ssize_t bridge_write(void *device, const unsigned char *data, size_t len);
extern ssize_t bridge_read(void *device, unsigned char *buf, size_t len);
extern void bridge_flush(void *device);
extern void bridge_log(void *job, int level, const char *msg);
extern bool bridge_is_canceled(void *job);
extern unsigned bridge_bytes_per_line(const void *options);
extern unsigned bridge_width_px(const void *options);
extern unsigned bridge_height_px(const void *options);
extern int bridge_get_print_darkness(const void *options);
extern int bridge_get_print_speed(const void *options);
extern unsigned bridge_get_media_tracking(const void *options);
extern int bridge_get_orientation(const void *options);
extern unsigned bridge_get_copies(const void *options);
extern unsigned bridge_get_bits_per_pixel(const void *options);
extern unsigned bridge_get_color_space(const void *options);
extern unsigned bridge_get_content_optimize(const void *options);
extern const char *bridge_get_vendor_option(const void *options, const char *name);
extern bool bridge_model_supports_compression(void *job);

static ssize_t ops_write(void *device, const uint8_t *data, size_t len) {
    return bridge_write(device, data, len);
}

static ssize_t ops_read(void *device, uint8_t *buf, size_t len) {
    return bridge_read(device, buf, len);
}

static void ops_flush(void *device) {
    bridge_flush(device);
}

static void ops_log(void *job, int level, const char *msg) {
    bridge_log(job, level, msg);
}

static bool ops_is_canceled(void *job) {
    return bridge_is_canceled(job);
}

static unsigned ops_bytes_per_line(const void *options) {
    return bridge_bytes_per_line(options);
}

static unsigned ops_width_px(const void *options) {
    return bridge_width_px(options);
}

static unsigned ops_height_px(const void *options) {
    return bridge_height_px(options);
}

static int ops_get_print_darkness(const void *options) {
    return bridge_get_print_darkness(options);
}

static int ops_get_print_speed(const void *options) {
    return bridge_get_print_speed(options);
}

static unsigned ops_get_media_tracking(const void *options) {
    return bridge_get_media_tracking(options);
}

static int ops_get_orientation(const void *options) {
    return bridge_get_orientation(options);
}

static unsigned ops_get_copies(const void *options) {
    return bridge_get_copies(options);
}

static unsigned ops_get_bits_per_pixel(const void *options) {
    return bridge_get_bits_per_pixel(options);
}

static unsigned ops_get_color_space(const void *options) {
    return bridge_get_color_space(options);
}

static unsigned ops_get_content_optimize(const void *options) {
    return bridge_get_content_optimize(options);
}

static const char *ops_get_vendor_option(const void *options, const char *name) {
    return bridge_get_vendor_option(options, name);
}

static bool ops_model_supports_compression(void *job) {
    return bridge_model_supports_compression(job);
}

static const PhomemoOps OPS = {
    .write            = ops_write,
    .read             = ops_read,
    .flush            = ops_flush,
    .log              = ops_log,
    .is_canceled      = ops_is_canceled,
    .bytes_per_line   = ops_bytes_per_line,
    .width_px         = ops_width_px,
    .height_px        = ops_height_px,
    .get_print_darkness  = ops_get_print_darkness,
    .get_print_speed     = ops_get_print_speed,
    .get_media_tracking  = ops_get_media_tracking,
    .get_orientation     = ops_get_orientation,
    .get_copies          = ops_get_copies,
    .get_bits_per_pixel  = ops_get_bits_per_pixel,
    .get_color_space     = ops_get_color_space,
    .get_content_optimize = ops_get_content_optimize,
    .get_vendor_option   = ops_get_vendor_option,
    .model_supports_compression = ops_model_supports_compression,
};

// ---------------------------------------------------------------------------
// Raster callbacks — delegate to Rust
// ---------------------------------------------------------------------------

static bool tp_rstartjob(pappl_job_t *job, pappl_pr_options_t *options,
                         pappl_device_t *device) {
    // Retrieve the print-head width from per-driver extension data.
    pappl_printer_t *printer = papplJobGetPrinter(job);
    pappl_pr_driver_data_t dd;
    papplPrinterGetDriverData(printer, &dd);

    unsigned head_w = 0;
    if (dd.extension) {
        const struct ModelInfoC *model_ext = (const struct ModelInfoC *)dd.extension;
        if (model_ext->max_width_bytes > 0)
            head_w = model_ext->max_width_bytes;
    }

    if (!head_w && options && options->header.cupsBytesPerLine > 0)
        head_w = options->header.cupsBytesPerLine;

    if (!head_w)
        return false;

    DriverCtx *ctx = pm_ctx_new(head_w);
    if (!ctx) return false;
    papplJobSetData(job, ctx);
    return pm_start_job(ctx, &OPS, job, options, device);
}

static bool tp_rstartpage(pappl_job_t *job, pappl_pr_options_t *options,
                          pappl_device_t *device, unsigned page) {
    DriverCtx *ctx = (DriverCtx *)papplJobGetData(job);
    return ctx && pm_start_page(ctx, &OPS, job, options, device, page);
}

static bool tp_rwriteline(pappl_job_t *job, pappl_pr_options_t *options,
                          pappl_device_t *device, unsigned y,
                          const unsigned char *line) {
    DriverCtx *ctx = (DriverCtx *)papplJobGetData(job);
    return ctx && pm_write_line(ctx, &OPS, job, options, device, y, line);
}

static bool tp_rendpage(pappl_job_t *job, pappl_pr_options_t *options,
                        pappl_device_t *device, unsigned page) {
    DriverCtx *ctx = (DriverCtx *)papplJobGetData(job);
    if (!ctx) return false;
    bool ok = pm_end_page(ctx, &OPS, job, options, device, page);
    if (ok) {
        unsigned copies = (options && options->copies > 0) ? options->copies : 1;
        papplJobSetImpressionsCompleted(job, (int)(page * copies));
    }
    return ok;
}

static bool tp_extract_print_result(const uint8_t *buffer, size_t len, bool *success) {
    if (!buffer || !success || len < 3)
        return false;

    for (size_t i = 0; i + 2 < len; i ++) {
        if (buffer[i] == 0x1a && buffer[i + 1] == 0x0f) {
            *success = (buffer[i + 2] == 0x0c);
            return true;
        }
    }

    return false;
}

static bool tp_rendjob(pappl_job_t *job, pappl_pr_options_t *options,
                       pappl_device_t *device) {
    DriverCtx *ctx = (DriverCtx *)papplJobGetData(job);
    bool ok = ctx && pm_end_job(ctx, &OPS, job, options, device);
    pm_ctx_free(ctx);
    papplJobSetData(job, NULL);

    // Wait for the printer's completion response (1A 0F 0C) with a
    // longer timeout than the default SO_RCVTIMEO.  This ensures the
    // RFCOMM send buffer is flushed and the printer has finished
    // printing before PAPPL closes the device.
    if (ok) {
        struct BtConnectionHandle *handle = papplDeviceGetData(device);
        if (handle) {
            uint8_t resp[64];
            ssize_t nread = pm_bt_read_timeout(handle, resp, sizeof(resp), 10000);
            bool success = false;

            if (nread <= 0) {
                papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                            "tp_rendjob: no print-complete response from device");
                ok = false;
            } else if (!tp_extract_print_result(resp, (size_t)nread, &success)) {
                papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                            "tp_rendjob: missing print-result packet in device response");
                ok = false;
            } else if (!success) {
                papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                            "tp_rendjob: printer reported print failure");
                ok = false;
            }
        }
    }

    return ok;
}

// ---------------------------------------------------------------------------
// Identify callback — no-op with log.
// ---------------------------------------------------------------------------

static void tp_identify(pappl_printer_t *printer,
                        pappl_identify_actions_t actions,
                        const char *message) {
    (void)actions;
    papplLogPrinter(printer, PAPPL_LOGLEVEL_INFO,
        "Identify requested: %s", message ? message : "(no message)");
}

// ---------------------------------------------------------------------------
// Printer status callback (idle polling)
// ---------------------------------------------------------------------------

static bool tp_status(pappl_printer_t *printer) {
    pappl_device_t *device = papplPrinterOpenDevice(printer);
    if (!device)
        return false;

    pappl_preason_t reasons = papplDeviceGetStatus(device);
    if (reasons & PAPPL_PREASON_MEDIA_EMPTY)
        reasons |= PAPPL_PREASON_MEDIA_NEEDED;

    papplPrinterCloseDevice(printer);

    pappl_preason_t clear = PAPPL_PREASON_COVER_OPEN | PAPPL_PREASON_MEDIA_EMPTY
                          | PAPPL_PREASON_MEDIA_NEEDED
                          | PAPPL_PREASON_MARKER_SUPPLY_LOW | PAPPL_PREASON_OFFLINE
                          | PAPPL_PREASON_OTHER;
    papplPrinterSetReasons(printer, reasons, clear & ~reasons);

    return true;
}

// ---------------------------------------------------------------------------
// Test page callback — generates a small test pattern
// ---------------------------------------------------------------------------

static const char *tp_testpage(pappl_printer_t *printer, char *buffer,
                               size_t bufsize) {
    int fd = papplCreateTempFile(buffer, bufsize, "phomemo-testpage", "png");
    if (fd < 0) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "tp_testpage: unable to create temporary PNG: %s",
                        strerror(errno));
        return NULL;
    }

    bool ok = pm_write_testpage_png(fd);
    if (close(fd) < 0) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "tp_testpage: close failed for temporary PNG: %s",
                        strerror(errno));
        ok = false;
    }

    if (!ok) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "tp_testpage: failed to write test page PNG");
        unlink(buffer);
        return NULL;
    }

    return buffer;
}

// ---------------------------------------------------------------------------
// printfile_cb — optional raw passthrough path
// ---------------------------------------------------------------------------

static bool tp_printfile(pappl_job_t *job, pappl_pr_options_t *options,
                         pappl_device_t *device) {
    (void)options;

    char filename[1024];
    int fd = papplJobOpenFile(job, filename, sizeof(filename), NULL, NULL, "r");
    if (fd < 0) {
        papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                    "tp_printfile: unable to open job file");
        return false;
    }

    unsigned char buffer[4096];
    bool ok = true;
    ssize_t nread;

    while ((nread = read(fd, buffer, sizeof(buffer))) > 0) {
        ssize_t nwritten = papplDeviceWrite(device, buffer, (size_t)nread);
        if (nwritten != nread) {
            papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                        "tp_printfile: short/failed write (%zd/%zd)",
                        nwritten, nread);
            ok = false;
            break;
        }
    }

    if (nread < 0) {
        papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                    "tp_printfile: read failed");
        ok = false;
    }

    close(fd);

    if (ok)
        papplDeviceFlush(device);

    return ok;
}

static void tp_add_driver_attrs(ipp_t **driver_attrs) {
    if (!driver_attrs)
        return;

    ipp_t *attrs = *driver_attrs;
    if (!attrs) {
        attrs = ippNew();
        if (!attrs)
            return;
        *driver_attrs = attrs;
    }

    static const char *const dither_values[] = {
        "auto",
        "threshold",
        "bayer8",
        "floyd-steinberg",
        "atkinson",
    };
    static const char *const compression_values[] = {
        "auto",
        "on",
        "off",
    };

    ippAddStrings(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        "phomemo-dither-supported",
        (int)(sizeof(dither_values) / sizeof(dither_values[0])),
        NULL,
        dither_values);
    ippAddString(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        "phomemo-dither-default",
        NULL,
        "auto");

    ippAddStrings(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        "phomemo-compression-supported",
        (int)(sizeof(compression_values) / sizeof(compression_values[0])),
        NULL,
        compression_values);
    ippAddString(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        "phomemo-compression-default",
        NULL,
        "auto");
}

// ---------------------------------------------------------------------------
// autoadd_cb — delegates to Rust IEEE 1284 scored matcher
// ---------------------------------------------------------------------------

const char *tp_autoadd_cb(const char *device_info,
                          const char *device_uri,
                          const char *device_id,
                          void *data) {
    (void)device_uri;
    (void)data;
    // Three-phase matching (exact MDL, scored 1284, fallback substring)
    // implemented in Rust — see pm_autoadd_match in lib.rs / ieee1284.rs.
    return pm_autoadd_match(device_info, device_id);
}

// ---------------------------------------------------------------------------
// driver_cb — thin wrapper: Rust fills data-driven values, C sets callbacks
// ---------------------------------------------------------------------------

bool tp_driver_cb(pappl_system_t *system, const char *driver_name,
                  const char *device_uri, const char *device_id,
                  pappl_pr_driver_data_t *dd, ipp_t **driver_attrs,
                  void *data) {
    (void)system; (void)device_uri; (void)device_id;
    (void)data;

    // Ask Rust for all data-driven defaults.
    struct DriverDefaultsC defs;
    memset(&defs, 0, sizeof(defs));
    if (!pm_driver_defaults(driver_name, &defs))
        return false;

    const struct ModelInfoC *model = pm_model_lookup(driver_name);
    if (!model)
        return false;

    memset(dd, 0, sizeof(*dd));

    // --- Callbacks (must stay C — they reference PAPPL opaque types) ---
    dd->rstartjob_cb  = tp_rstartjob;
    dd->rstartpage_cb = tp_rstartpage;
    dd->rwriteline_cb = tp_rwriteline;
    dd->rendpage_cb   = tp_rendpage;
    dd->rendjob_cb    = tp_rendjob;
    dd->status_cb     = tp_status;
    dd->identify_cb   = tp_identify;
    dd->testpage_cb   = tp_testpage;
    dd->printfile_cb  = tp_printfile;

    // --- Data-driven values from Rust ---
    memcpy(dd->make_and_model, defs.make_and_model, sizeof(dd->make_and_model));

    dd->kind = PAPPL_KIND_LABEL | PAPPL_KIND_ROLL;
    dd->has_supplies = true;
    dd->ppm = 1;

    // Color & raster — accept grayscale input for Rust-side dithering.
    // CUPS/GIMP send "monochrome" or "auto"; we must accept both.
    dd->color_supported   = PAPPL_COLOR_MODE_AUTO
                          | PAPPL_COLOR_MODE_AUTO_MONOCHROME
                          | PAPPL_COLOR_MODE_BI_LEVEL
                          | PAPPL_COLOR_MODE_MONOCHROME;
    dd->color_default     = PAPPL_COLOR_MODE_AUTO_MONOCHROME;
    dd->content_default   = PAPPL_CONTENT_AUTO;
    dd->quality_default   = IPP_QUALITY_NORMAL;
    dd->scaling_default   = PAPPL_SCALING_AUTO;
    dd->raster_types      = PAPPL_PWG_RASTER_TYPE_BLACK_1
                          | PAPPL_PWG_RASTER_TYPE_BLACK_8
                          | PAPPL_PWG_RASTER_TYPE_SGRAY_8;
    dd->force_raster_type = PAPPL_PWG_RASTER_TYPE_NONE;
    dd->sides_supported   = PAPPL_SIDES_ONE_SIDED;
    dd->sides_default     = PAPPL_SIDES_ONE_SIDED;
    dd->orient_default    = IPP_ORIENT_NONE;

    // Resolution
    dd->num_resolution = 1;
    dd->x_resolution[0] = dd->y_resolution[0] = defs.dpi;
    dd->x_default = dd->y_default = defs.dpi;

    // Margins — borderless
    dd->borderless = true;

    // Darkness
    dd->darkness_supported  = defs.darkness_supported;
    dd->darkness_default    = defs.darkness_default;
    dd->darkness_configured = defs.darkness_default;

    // Speed
    dd->speed_supported[0] = defs.speed_min;
    dd->speed_supported[1] = defs.speed_max;
    dd->speed_default      = defs.speed_default;

    // Label modes
    dd->mode_supported = PAPPL_LABEL_MODE_TEAR_OFF
                       | (defs.has_cutter ? PAPPL_LABEL_MODE_CUTTER : 0);
    dd->mode_configured = defs.has_cutter
                        ? PAPPL_LABEL_MODE_CUTTER
                        : PAPPL_LABEL_MODE_TEAR_OFF;
    unsigned tracking_supported = defs.tracking_supported;
    for (int i = 0; i < defs.num_media && i < PAPPL_MAX_MEDIA; i++)
        tracking_supported |= defs.media_tracking[i];
    if (!tracking_supported)
        tracking_supported = PAPPL_MEDIA_TRACKING_CONTINUOUS
                           | PAPPL_MEDIA_TRACKING_GAP
                           | PAPPL_MEDIA_TRACKING_MARK;
    dd->tracking_supported = tracking_supported;

    dd->tear_offset_supported[0] = -500;
    dd->tear_offset_supported[1] =  500;

    dd->identify_supported = PAPPL_IDENTIFY_ACTIONS_NONE;
    dd->identify_default   = PAPPL_IDENTIFY_ACTIONS_NONE;

    // Media list from Rust
    dd->num_media = (defs.num_media <= PAPPL_MAX_MEDIA) ? defs.num_media : PAPPL_MAX_MEDIA;
    for (int i = 0; i < dd->num_media; i++)
        dd->media[i] = defs.media_names[i];

    // Default media
    dd->media_default.size_width  = defs.default_width;
    dd->media_default.size_length = defs.default_length;
    dd->media_default.tracking    = defs.default_tracking ? defs.default_tracking : PAPPL_MEDIA_TRACKING_GAP;
    memcpy(dd->media_default.size_name, defs.default_size_name,
           sizeof(dd->media_default.size_name));
    snprintf(dd->media_default.source, sizeof(dd->media_default.source), "main-roll");
    snprintf(
        dd->media_default.type,
        sizeof(dd->media_default.type),
        "%s",
        (dd->media_default.size_length == 0) ? "continuous" : "labels");

    dd->num_source = 1;
    dd->source[0]  = "main-roll";
    dd->media_ready[0] = dd->media_default;
    dd->num_type = 2;
    dd->type[0]  = "labels";
    dd->type[1]  = "continuous";
    dd->num_bin = 1;
    dd->bin[0]  = "face-up";

    dd->format = "application/vnd.phomemo-raw";

    // Vendor attribute: dithering algorithm selection
    dd->num_vendor = 2;
    dd->vendor[0]  = "phomemo-dither";
    dd->vendor[1]  = "phomemo-compression";
    tp_add_driver_attrs(driver_attrs);

    // Per-driver extension → immutable model metadata.
    dd->extension = (void *)model;

    return true;
}
