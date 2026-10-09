//
// Phomemo Printer Application — driver callbacks.
//
// phomemo_driver_cb reports each model's capabilities to PAPPL. The raster
// callbacks hand PAPPL's pages to the Rust driver (pm_job_*), together with
// driver_ops, the PAPPL functions it calls back.
//

#include <errno.h>
#include <string.h>
#include <unistd.h>
#include "phomemo.h"

// ---------------------------------------------------------------------------
// The PAPPL and CUPS values the Rust side mirrors (src/pappl.rs) must be the
// real ones.
// ---------------------------------------------------------------------------

#define ASSERT_SAME(rust, pappl) \
    _Static_assert((rust) == (pappl), #rust " differs from " #pappl)

ASSERT_SAME(PM_MAX_MEDIA, PAPPL_MAX_MEDIA);
ASSERT_SAME(PM_LOGLEVEL_DEBUG, PAPPL_LOGLEVEL_DEBUG);
ASSERT_SAME(PM_LOGLEVEL_INFO, PAPPL_LOGLEVEL_INFO);
ASSERT_SAME(PM_LOGLEVEL_WARN, PAPPL_LOGLEVEL_WARN);
ASSERT_SAME(PM_LOGLEVEL_ERROR, PAPPL_LOGLEVEL_ERROR);
ASSERT_SAME(PM_MEDIA_TRACKING_CONTINUOUS, PAPPL_MEDIA_TRACKING_CONTINUOUS);
ASSERT_SAME(PM_MEDIA_TRACKING_GAP, PAPPL_MEDIA_TRACKING_GAP);
ASSERT_SAME(PM_MEDIA_TRACKING_MARK, PAPPL_MEDIA_TRACKING_MARK);
ASSERT_SAME(PM_PREASON_OTHER, PAPPL_PREASON_OTHER);
ASSERT_SAME(PM_PREASON_COVER_OPEN, PAPPL_PREASON_COVER_OPEN);
ASSERT_SAME(PM_PREASON_MARKER_SUPPLY_LOW, PAPPL_PREASON_MARKER_SUPPLY_LOW);
ASSERT_SAME(PM_PREASON_MEDIA_EMPTY, PAPPL_PREASON_MEDIA_EMPTY);
ASSERT_SAME(PM_PREASON_OFFLINE, PAPPL_PREASON_OFFLINE);
ASSERT_SAME(PM_COLOR_MODE_BI_LEVEL, PAPPL_COLOR_MODE_BI_LEVEL);
ASSERT_SAME(PM_CONTENT_TEXT, PAPPL_CONTENT_TEXT);
ASSERT_SAME(PM_CONTENT_TEXT_AND_GRAPHIC, PAPPL_CONTENT_TEXT_AND_GRAPHIC);
ASSERT_SAME(PM_CSPACE_W, CUPS_CSPACE_W);
ASSERT_SAME(PM_CSPACE_K, CUPS_CSPACE_K);
ASSERT_SAME(PM_CSPACE_SW, CUPS_CSPACE_SW);
// The job context holds the ready media's whole name.
ASSERT_SAME(sizeof(((PmJobContext *)0)->ready_size_name),
            sizeof(((pappl_media_col_t *)0)->size_name));

// ---------------------------------------------------------------------------
// The PAPPL functions the Rust driver calls back
// ---------------------------------------------------------------------------

static void driver_log(pappl_job_t *job, int level, const char *message) {
    papplLogJob(job, (pappl_loglevel_t)level, "%s", message);
}

static PmOptions driver_options(const pappl_pr_options_t *options) {
    return (PmOptions) {
        .width               = options->header.cupsWidth,
        .height              = options->header.cupsHeight,
        .bytes_per_line      = options->header.cupsBytesPerLine,
        .bits_per_pixel      = options->header.cupsBitsPerPixel,
        .color_space         = options->header.cupsColorSpace,
        .print_darkness      = options->print_darkness,
        .darkness_configured = options->darkness_configured,
        .print_speed         = options->print_speed,
        .media_tracking      = options->media.tracking,
        .media_length        = options->media.size_length,
        .color_mode          = options->print_color_mode,
        .content_optimize    = options->print_content_optimize,
        .dither              = cupsGetOption(VENDOR_DITHER,
                                             options->num_vendor, options->vendor),
        .compression         = cupsGetOption(VENDOR_COMPRESSION,
                                             options->num_vendor, options->vendor),
        .media_size_name     = options->media.size_name,
        .media_width         = options->media.size_width,
        .cups_page_size      = { options->header.cupsPageSize[0],
                                 options->header.cupsPageSize[1] },
        .page_size           = { options->header.PageSize[0],
                                 options->header.PageSize[1] },
        .resolution          = { options->header.HWResolution[0],
                                 options->header.HWResolution[1] },
        .overprint_vertical  = cupsGetOption(VENDOR_OVERPRINT_VERTICAL,
                                             options->num_vendor, options->vendor),
    };
}

static const PmOps driver_ops = {
    .write       = papplDeviceWrite,
    .flush       = papplDeviceFlush,
    .is_canceled = papplJobIsCanceled,
    .log         = driver_log,
    .options     = driver_options,
};

// ---------------------------------------------------------------------------
// Raster callbacks — delegate to Rust
// ---------------------------------------------------------------------------

// PAPPL 1.4 looks vendor defaults up in the job's attributes only, so the
// driver applies them itself.

// Copy `default_name`'s value from the printer's driver attributes `attrs`
// (NULL allowed) into `value`; empty if there is none.
static void vendor_default(ipp_t *attrs, const char *default_name, char *value,
                           size_t size) {
    value[0] = '\0';
    ipp_attribute_t *attr = attrs ? ippFindAttribute(attrs, default_name, IPP_TAG_ZERO)
                                  : NULL;
    const char *found = attr ? ippGetString(attr, 0, NULL) : NULL;
    if (found)
        papplCopyString(value, found, size);
}

void phomemo_vendor_default(pappl_printer_t *printer, const char *default_name,
                            char *value, size_t size) {
    // A copy, which is the caller's to delete.
    ipp_t *attrs = papplPrinterGetDriverAttributes(printer);
    vendor_default(attrs, default_name, value, size);
    ippDelete(attrs);
}

static bool driver_rstartjob(pappl_job_t *job, pappl_pr_options_t *options,
                             pappl_device_t *device) {
    pappl_printer_t *printer = papplJobGetPrinter(job);
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    // Read once per job: the loaded media and the printer's defaults.
    PmJobContext context = {
        .ready_width    = data.media_ready[0].size_width,
        .ready_length   = data.media_ready[0].size_length,
        .ready_tracking = data.media_ready[0].tracking,
    };
    papplCopyString(context.ready_size_name, data.media_ready[0].size_name,
                    sizeof(context.ready_size_name));
    // One copy of the driver attributes for all three defaults.
    ipp_t *attrs = papplPrinterGetDriverAttributes(printer);
    vendor_default(attrs, VENDOR_OVERPRINT_VERTICAL "-default",
                   context.overprint_vertical_default,
                   sizeof(context.overprint_vertical_default));
    vendor_default(attrs, VENDOR_DITHER "-default", context.dither_default,
                   sizeof(context.dither_default));
    vendor_default(attrs, VENDOR_COMPRESSION "-default", context.compression_default,
                   sizeof(context.compression_default));
    ippDelete(attrs);

    PmJob *ctx = pm_job_start(data.extension, context, &driver_ops, job, options,
                              device);
    papplJobSetData(job, ctx);
    if (ctx)
        phomemo_bt_start_job(job, device);
    return ctx != NULL;
}

static bool driver_rstartpage(pappl_job_t *job, pappl_pr_options_t *options,
                              pappl_device_t *device, unsigned page) {
    (void)page;
    return pm_job_start_page(papplJobGetData(job), &driver_ops, job, options, device);
}

static bool driver_rwriteline(pappl_job_t *job, pappl_pr_options_t *options,
                              pappl_device_t *device, unsigned y,
                              const unsigned char *line) {
    (void)y;
    return pm_job_write_line(papplJobGetData(job), &driver_ops, job, options, device, line);
}

static bool driver_rendpage(pappl_job_t *job, pappl_pr_options_t *options,
                            pappl_device_t *device, unsigned page) {
    (void)page;
    return pm_job_end_page(papplJobGetData(job), &driver_ops, job, options, device);
}

static bool driver_rendjob(pappl_job_t *job, pappl_pr_options_t *options,
                           pappl_device_t *device) {
    // PAPPL ends a job again when ending it failed; it has ended already.
    PmJob *ctx = papplJobGetData(job);
    if (!ctx)
        return false;

    PmJobSent sent = pm_job_sent(ctx);

    // pm_job_end frees the job's driver state, whatever it returns.
    bool ok = pm_job_end(ctx, &driver_ops, job, options, device);
    papplJobSetData(job, NULL);

    // The printer reports each page it has printed; PAPPL closes the device
    // once the job has ended.
    return ok && phomemo_bt_wait_printed(job, device, sent);
}

// ---------------------------------------------------------------------------
// Identify callback — no-op with log.
// ---------------------------------------------------------------------------

static void driver_identify(pappl_printer_t *printer,
                            pappl_identify_actions_t actions,
                            const char *message) {
    (void)actions;
    papplLogPrinter(printer, PAPPL_LOGLEVEL_INFO,
        "Identify requested: %s", message ? message : "(no message)");
}

// ---------------------------------------------------------------------------
// Printer status callback (idle polling)
// ---------------------------------------------------------------------------

static bool driver_status(pappl_printer_t *printer) {
    pappl_device_t *device = papplPrinterOpenDevice(printer);
    if (!device)
        return false;

    pappl_preason_t reasons = papplDeviceGetStatus(device);
    if (reasons & PAPPL_PREASON_MEDIA_EMPTY)
        reasons |= PAPPL_PREASON_MEDIA_NEEDED;

    // The battery of a Bluetooth printer; other devices report none.
    pappl_supply_t supply;
    if (papplDeviceGetSupplies(device, 1, &supply) == 1)
        papplPrinterSetSupplies(printer, 1, &supply);

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

static const char *driver_testpage(pappl_printer_t *printer, char *buffer,
                                   size_t bufsize) {
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    int fd = papplCreateTempFile(buffer, bufsize, "phomemo-testpage", "png");
    if (fd < 0) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "Unable to create the test page file: %s",
                        strerror(errno));
        return NULL;
    }

    bool ok = pm_write_testpage_png(fd, data.extension, data.media_ready[0].size_width,
                                    data.media_ready[0].size_length);
    if (close(fd) < 0) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "Unable to close the test page file: %s",
                        strerror(errno));
        ok = false;
    }

    if (!ok) {
        papplLogPrinter(printer, PAPPL_LOGLEVEL_ERROR,
                        "Unable to write the test page.");
        unlink(buffer);
        return NULL;
    }

    return buffer;
}

// ---------------------------------------------------------------------------
// printfile_cb — optional raw passthrough path
// ---------------------------------------------------------------------------

static bool driver_printfile(pappl_job_t *job, pappl_pr_options_t *options,
                             pappl_device_t *device) {
    (void)options;

    char filename[1024];
    int fd = papplJobOpenFile(job, filename, sizeof(filename), NULL, NULL, "r");
    if (fd < 0) {
        papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                    "Unable to open the job file: %s", strerror(errno));
        return false;
    }

    unsigned char buffer[4096];
    bool ok = true;
    ssize_t nread;

    while ((nread = read(fd, buffer, sizeof(buffer))) > 0) {
        ssize_t nwritten = papplDeviceWrite(device, buffer, (size_t)nread);
        if (nwritten != nread) {
            papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                        "Unable to send the job: wrote %zd of %zd bytes.",
                        nwritten, nread);
            ok = false;
            break;
        }
    }

    if (nread < 0) {
        papplLogJob(job, PAPPL_LOGLEVEL_ERROR,
                    "Unable to read the job file: %s", strerror(errno));
        ok = false;
    }

    close(fd);

    if (ok)
        papplDeviceFlush(device);

    return ok;
}

// Add the vendor attributes' supported and default values; those of
// phomemo-overprint-vertical only if `overprint`, the model has canvases.
static void driver_add_vendor_attrs(ipp_t **driver_attrs, bool overprint) {
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
        VENDOR_DITHER "-supported",
        (int)(sizeof(dither_values) / sizeof(dither_values[0])),
        NULL,
        dither_values);
    ippAddString(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        VENDOR_DITHER "-default",
        NULL,
        "auto");

    ippAddStrings(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        VENDOR_COMPRESSION "-supported",
        (int)(sizeof(compression_values) / sizeof(compression_values[0])),
        NULL,
        compression_values);
    ippAddString(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        VENDOR_COMPRESSION "-default",
        NULL,
        "auto");

    if (!overprint)
        return;

    // The policies and their default come from Rust (VerticalPolicy).
    const char *vertical_values[8];
    int num_vertical = 0;
    for (const char *keyword;
         num_vertical < (int)(sizeof(vertical_values) / sizeof(vertical_values[0])) &&
         (keyword = pm_overprint_vertical_keyword((unsigned)num_vertical)) != NULL;)
        vertical_values[num_vertical++] = keyword;

    ippAddStrings(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        VENDOR_OVERPRINT_VERTICAL "-supported",
        num_vertical,
        NULL,
        vertical_values);
    ippAddString(
        attrs,
        IPP_TAG_PRINTER,
        IPP_TAG_KEYWORD,
        VENDOR_OVERPRINT_VERTICAL "-default",
        NULL,
        pm_overprint_vertical_default());
}

// ---------------------------------------------------------------------------
// Auto-add callback — delegates to the Rust IEEE 1284 matcher
// ---------------------------------------------------------------------------

const char *phomemo_autoadd_cb(const char *device_info,
                               const char *device_uri,
                               const char *device_id,
                               void *data) {
    (void)device_uri;
    (void)data;
    // Matching is implemented in Rust: see phomemo-pappl/src/autoadd.rs.
    return pm_autoadd_match(device_info, device_id);
}

// ---------------------------------------------------------------------------
// Driver callback — Rust supplies the model's capabilities, C the rest
// ---------------------------------------------------------------------------

bool phomemo_driver_cb(pappl_system_t *system, const char *driver_name,
                       const char *device_uri, const char *device_id,
                       pappl_pr_driver_data_t *dd, ipp_t **driver_attrs,
                       void *data) {
    (void)system; (void)device_uri; (void)device_id;
    (void)data;

    // PAPPL has already initialized dd with its own defaults.
    const PmModel *model = pm_model_lookup(driver_name);
    PmDriverDefaults defaults;
    if (!pm_driver_defaults(model, &defaults))
        return false;

    // --- Callbacks (must stay C — they reference PAPPL opaque types) ---
    dd->rstartjob_cb  = driver_rstartjob;
    dd->rstartpage_cb = driver_rstartpage;
    dd->rwriteline_cb = driver_rwriteline;
    dd->rendpage_cb   = driver_rendpage;
    dd->rendjob_cb    = driver_rendjob;
    dd->status_cb     = driver_status;
    dd->identify_cb   = driver_identify;
    dd->testpage_cb   = driver_testpage;
    dd->printfile_cb  = driver_printfile;

    papplCopyString(dd->make_and_model, defaults.make_and_model,
                    sizeof(dd->make_and_model));

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
    // 8-bit only: PAPPL 1.4 hands a client's black_1 lines to the driver
    // while the page header still describes its own 8-bit raster
    // (_papplJobProcessRaster replaces the header only when both are 8-bit),
    // so 1-bit input cannot be told apart. bi-level is thresholded in Rust.
    dd->raster_types      = PAPPL_PWG_RASTER_TYPE_BLACK_8
                          | PAPPL_PWG_RASTER_TYPE_SGRAY_8;
    dd->force_raster_type = PAPPL_PWG_RASTER_TYPE_NONE;
    dd->sides_supported   = PAPPL_SIDES_ONE_SIDED;
    dd->sides_default     = PAPPL_SIDES_ONE_SIDED;
    dd->orient_default    = IPP_ORIENT_NONE;

    // Resolution
    dd->num_resolution = 1;
    dd->x_resolution[0] = dd->y_resolution[0] = defaults.dpi;
    dd->x_default = dd->y_default = defaults.dpi;

    // Margins — borderless
    dd->borderless = true;

    dd->darkness_supported  = defaults.darkness_supported;
    dd->darkness_configured = defaults.darkness_configured;
    dd->darkness_default    = defaults.darkness_default;

    dd->speed_supported[0] = defaults.speed_supported[0];
    dd->speed_supported[1] = defaults.speed_supported[1];
    dd->speed_default      = defaults.speed_default;

    // Label modes
    dd->mode_supported = PAPPL_LABEL_MODE_TEAR_OFF
                       | (defaults.has_cutter ? PAPPL_LABEL_MODE_CUTTER : 0);
    dd->mode_configured = defaults.has_cutter
                        ? PAPPL_LABEL_MODE_CUTTER
                        : PAPPL_LABEL_MODE_TEAR_OFF;
    dd->tracking_supported = defaults.tracking_supported;

    dd->tear_offset_supported[0] = -500;
    dd->tear_offset_supported[1] =  500;

    dd->identify_supported = PAPPL_IDENTIFY_ACTIONS_NONE;
    dd->identify_default   = PAPPL_IDENTIFY_ACTIONS_NONE;

    // Media list from Rust: the catalog's sizes, the overprint canvases,
    // then the custom range's bounds.
    _Static_assert(sizeof(dd->media) == sizeof(defaults.media),
                   "PmDriverDefaults.media must match pappl_pr_driver_data_t.media");
    dd->num_media = defaults.num_media;
    memcpy(dd->media, defaults.media, sizeof(dd->media));

    // Default media
    papplCopyString(dd->media_default.size_name, defaults.media_default.size_name,
                    sizeof(dd->media_default.size_name));
    dd->media_default.size_width  = defaults.media_default.width;
    dd->media_default.size_length = defaults.media_default.length;
    dd->media_default.tracking    = defaults.media_default.tracking;
    papplCopyString(dd->media_default.type, defaults.media_default.media_type,
                    sizeof(dd->media_default.type));
    papplCopyString(dd->media_default.source, "main-roll",
                    sizeof(dd->media_default.source));

    dd->num_source = 1;
    dd->source[0]  = "main-roll";
    dd->media_ready[0] = dd->media_default;
    dd->num_type = 2;
    dd->type[0]  = "labels";
    dd->type[1]  = "continuous";
    dd->num_bin = 1;
    dd->bin[0]  = "face-up";

    dd->format = "application/vnd.phomemo-raw";

    // Vendor attributes: dithering algorithm and compression selection, and
    // on a model with overprint canvases, the vertical policy.
    bool overprint = pm_overprint_count(model) > 0;
    dd->num_vendor = 2;
    dd->vendor[0]  = VENDOR_DITHER;
    dd->vendor[1]  = VENDOR_COMPRESSION;
    if (overprint)
        dd->vendor[dd->num_vendor++] = VENDOR_OVERPRINT_VERTICAL;
    driver_add_vendor_attrs(driver_attrs, overprint);

    // Per-driver extension: the model, for the raster callbacks, the test
    // page and the media page. PAPPL only stores the (non-const) pointer.
    dd->extension = (void *)model;

    return true;
}
