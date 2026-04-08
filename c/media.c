//
// Custom media management page and persistence for Phomemo printers.
//
// Single-roll model: one media source ("main-roll"), with custom size
// selection and persistence via papplPrinterOpenFile.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <pappl/pappl.h>
#include "phomemo_pappl.h"

static void media_format_mm(int hundredths_mm, char *buffer, size_t bufsize) {
    if (!buffer || bufsize == 0)
        return;

    if (hundredths_mm % 100 == 0)
        snprintf(buffer, bufsize, "%d", hundredths_mm / 100);
    else
        snprintf(buffer, bufsize, "%.2f", hundredths_mm / 100.0);
}

static bool media_parse_decimal(const char *value, double *out) {
    if (!value || !*value || !out)
        return false;

    char *end = NULL;
    errno = 0;
    double parsed = strtod(value, &end);
    if (errno != 0 || !end || *end)
        return false;

    *out = parsed;
    return true;
}

static int media_head_max_width_hundredths_mm(const pappl_pr_driver_data_t *data) {
    if (!data || !data->extension)
        return 0;

    const struct ModelInfoC *model = (const struct ModelInfoC *)data->extension;
    if (!model || model->max_width_px == 0 || model->dpi == 0)
        return 0;

    unsigned width_hundredths =
        (unsigned)model->max_width_px * 2540u + (unsigned)model->dpi / 2u;
    width_hundredths /= (unsigned)model->dpi;

    return (int)width_hundredths;
}

static bool media_tracking_is_supported(unsigned tracking_supported, int tracking) {
    return (tracking == PAPPL_MEDIA_TRACKING_CONTINUOUS &&
            (tracking_supported & PAPPL_MEDIA_TRACKING_CONTINUOUS)) ||
           (tracking == PAPPL_MEDIA_TRACKING_GAP &&
            (tracking_supported & PAPPL_MEDIA_TRACKING_GAP)) ||
           (tracking == PAPPL_MEDIA_TRACKING_MARK &&
            (tracking_supported & PAPPL_MEDIA_TRACKING_MARK));
}

static void media_format_option_label(const char *size_name, char *buffer, size_t bufsize) {
    if (!buffer || bufsize == 0)
        return;

    if (!size_name || !*size_name) {
        buffer[0] = '\0';
        return;
    }

    pwg_media_t *pwg = pwgMediaForPWG(size_name);
    if (!pwg) {
        papplCopyString(buffer, size_name, bufsize);
        return;
    }

    char width[32], length[32];
    media_format_mm(pwg->width, width, sizeof(width));
    media_format_mm(pwg->length, length, sizeof(length));

    if (pwg->length == 0) {
        snprintf(buffer, bufsize, "%s mm continuous roll", width);
    } else {
        snprintf(buffer, bufsize, "%s x %s mm label", width, length);
    }
}

static bool media_get_web_resource(
    pappl_printer_t *printer,
    const char *subpath,
    char *buffer,
    size_t bufsize) {
    char ipp_path[256];
    if (!papplPrinterGetPath(printer, "", ipp_path, sizeof(ipp_path)))
        return false;

    const char *slug = strrchr(ipp_path, '/');
    if (!slug || !slug[1])
        return false;
    slug ++;

    if (subpath && *subpath)
        snprintf(buffer, bufsize, "/%s/%s", slug, subpath);
    else
        snprintf(buffer, bufsize, "/%s", slug);
    return true;
}

static void media_normalize_ready_state(pappl_printer_t *printer) {
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    unsigned ready_tracking = 0;
    if (data.extension) {
        const struct ModelInfoC *model = (const struct ModelInfoC *)data.extension;
        if (model->driver_name)
            ready_tracking = pm_media_tracking_for_size(model->driver_name, data.media_ready[0].size_name);
    }
    if (!ready_tracking)
        ready_tracking = (data.media_ready[0].size_length == 0)
            ? PAPPL_MEDIA_TRACKING_CONTINUOUS
            : PAPPL_MEDIA_TRACKING_GAP;

    unsigned default_tracking = 0;
    if (data.extension) {
        const struct ModelInfoC *model = (const struct ModelInfoC *)data.extension;
        if (model->driver_name)
            default_tracking = pm_media_tracking_for_size(model->driver_name, data.media_default.size_name);
    }
    if (!default_tracking)
        default_tracking = (data.media_default.size_length == 0)
            ? PAPPL_MEDIA_TRACKING_CONTINUOUS
            : PAPPL_MEDIA_TRACKING_GAP;

    bool changed = false;
    if (data.media_ready[0].tracking != (int)ready_tracking) {
        data.media_ready[0].tracking = (int)ready_tracking;
        changed = true;
    }
    if (data.media_ready[0].size_length == 0) {
        if (strcmp(data.media_ready[0].type, "continuous")) {
            papplCopyString(data.media_ready[0].type, "continuous", sizeof(data.media_ready[0].type));
            changed = true;
        }
    } else if (strcmp(data.media_ready[0].type, "labels")) {
        papplCopyString(data.media_ready[0].type, "labels", sizeof(data.media_ready[0].type));
        changed = true;
    }

    if (data.media_default.tracking != (int)default_tracking) {
        data.media_default.tracking = (int)default_tracking;
        changed = true;
    }
    if (data.media_default.size_length == 0) {
        if (strcmp(data.media_default.type, "continuous")) {
            papplCopyString(data.media_default.type, "continuous", sizeof(data.media_default.type));
            changed = true;
        }
    } else if (strcmp(data.media_default.type, "labels")) {
        papplCopyString(data.media_default.type, "labels", sizeof(data.media_default.type));
        changed = true;
    }

    if (!changed)
        return;

    papplPrinterSetDriverData(printer, &data, NULL);
    papplPrinterSetReadyMedia(printer, 1, data.media_ready);
}

// ---------------------------------------------------------------------------
// Custom media file I/O
// ---------------------------------------------------------------------------

// Load the persisted custom media name for the printer.
// Returns true if a saved value was loaded into buffer.
static bool media_load(pappl_printer_t *printer, char *buffer, size_t bufsize) {
    char fname[1024];
    int fd = papplPrinterOpenFile(printer, fname, sizeof(fname), NULL,
                                  "custom-media", "txt", "r");
    if (fd < 0)
        return false;

    FILE *fp = fdopen(fd, "r");
    if (!fp) {
        close(fd);
        return false;
    }

    bool ok = (fgets(buffer, (int)bufsize, fp) != NULL);
    fclose(fp);

    // Strip trailing newline/whitespace
    if (ok) {
        size_t len = strlen(buffer);
        while (len > 0 && (buffer[len - 1] == '\n' || buffer[len - 1] == '\r'
                           || buffer[len - 1] == ' '))
            buffer[--len] = '\0';
        if (!*buffer) ok = false;
    }

    return ok;
}

// Save the current custom media name for the printer.
static bool media_save(pappl_printer_t *printer, const char *size_name) {
    char fname[1024];

    if (!size_name || !*size_name) {
        // Delete any existing file
        papplPrinterOpenFile(printer, fname, sizeof(fname), NULL,
                             "custom-media", "txt", "x");
        return true;
    }

    int fd = papplPrinterOpenFile(printer, fname, sizeof(fname), NULL,
                                  "custom-media", "txt", "w");
    if (fd < 0)
        return false;

    FILE *fp = fdopen(fd, "w");
    if (!fp) {
        close(fd);
        return false;
    }

    fprintf(fp, "%s\n", size_name);
    fclose(fp);
    return true;
}

// ---------------------------------------------------------------------------
// Apply loaded media to driver data + ready media
// ---------------------------------------------------------------------------

static void media_apply(pappl_printer_t *printer, const char *size_name) {
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    pwg_media_t *pwg = pwgMediaForPWG(size_name);
    if (!pwg)
        return;

    papplCopyString(data.media_ready[0].size_name, size_name,
                    sizeof(data.media_ready[0].size_name));
    data.media_ready[0].size_width  = pwg->width;
    data.media_ready[0].size_length = pwg->length;
    papplCopyString(
        data.media_ready[0].type,
        (pwg->length == 0) ? "continuous" : "labels",
        sizeof(data.media_ready[0].type));

    // Preferred tracking from Rust media catalog; fallback to length heuristic.
    unsigned tracking = 0;
    if (data.extension) {
        const struct ModelInfoC *model = (const struct ModelInfoC *)data.extension;
        if (model->driver_name)
            tracking = pm_media_tracking_for_size(model->driver_name, size_name);
    }
    if (!tracking) {
        tracking = (pwg->length > 0)
            ? PAPPL_MEDIA_TRACKING_GAP
            : PAPPL_MEDIA_TRACKING_CONTINUOUS;
    }
    data.media_ready[0].tracking = tracking;

    papplPrinterSetDriverData(printer, &data, NULL);
    papplPrinterSetReadyMedia(printer, 1, data.media_ready);
}

// ---------------------------------------------------------------------------
// Web page handler
// ---------------------------------------------------------------------------

static bool media_page_cb(pappl_client_t *client, void *data) {
    pappl_printer_t *printer = (pappl_printer_t *)data;
    if (!printer)
        return false;

    if (!papplClientHTMLAuthorize(client))
        return true;

    pappl_pr_driver_data_t ddata;
    papplPrinterGetDriverData(printer, &ddata);

    const char *status = NULL;

    // Handle POST — form submission
    if (papplClientGetMethod(client) == HTTP_STATE_POST) {
        int num_form = 0;
        cups_option_t *form = NULL;

        num_form = papplClientGetForm(client, &form);
        if (num_form == 0) {
            status = "Invalid form data.";
        } else if (!papplClientIsValidForm(client, num_form, form)) {
            status = "Invalid form submission.";
        } else {
            const char *sel = cupsGetOption("media-size", num_form, form);
            if (sel && *sel) {
                if (!strcmp(sel, "custom")) {
                    // Custom size entered manually
                    const char *w_str = cupsGetOption("custom-width", num_form, form);
                    const char *l_str = cupsGetOption("custom-length", num_form, form);
                    const char *units = cupsGetOption("custom-units", num_form, form);

                    if (w_str && l_str && units) {
                        double width_value = 0.0;
                        double length_value = 0.0;
                        int width = 0;
                        int length = 0;
                        int max_width = media_head_max_width_hundredths_mm(&ddata);

                        if (!media_parse_decimal(w_str, &width_value) ||
                            !media_parse_decimal(l_str, &length_value) ||
                            width_value <= 0.0 || length_value < 0.0 ||
                            length_value > 2000.0) {
                            status = "Invalid dimensions.";
                            goto media_post_tracking;
                        }

                        if (!strcmp(units, "in")) {
                            width  = (int)(2540.0 * width_value + 0.5);
                            length = (int)(2540.0 * length_value + 0.5);
                        } else {
                            width  = (int)(100.0 * width_value + 0.5);
                            length = (int)(100.0 * length_value + 0.5);
                        }

                        if (width <= 0 || (max_width > 0 && width > max_width)) {
                            status = "Custom width exceeds printer capacity.";
                            goto media_post_tracking;
                        }

                        char name[128];
                        pwgFormatSizeName(name, sizeof(name), "custom",
                                          NULL, width, length, units);
                        media_apply(printer, name);
                        media_save(printer, name);
                        status = "Custom media saved.";
                    }
                } else {
                    // Standard size selected from dropdown
                    media_apply(printer, sel);
                    media_save(printer, sel);
                    status = "Media saved.";
                }
            }

            // Tracking
media_post_tracking:
            const char *track = cupsGetOption("media-tracking", num_form, form);
            if (track) {
                papplPrinterGetDriverData(printer, &ddata);
                int selected_tracking = 0;
                if (!strcmp(track, "continuous"))
                    selected_tracking = PAPPL_MEDIA_TRACKING_CONTINUOUS;
                else if (!strcmp(track, "gap"))
                    selected_tracking = PAPPL_MEDIA_TRACKING_GAP;
                else if (!strcmp(track, "mark"))
                    selected_tracking = PAPPL_MEDIA_TRACKING_MARK;

                if (selected_tracking &&
                    media_tracking_is_supported((unsigned)ddata.tracking_supported,
                                               selected_tracking)) {
                    ddata.media_ready[0].tracking = selected_tracking;
                    papplPrinterSetDriverData(printer, &ddata, NULL);
                    papplPrinterSetReadyMedia(printer, 1, ddata.media_ready);
                } else if (selected_tracking) {
                    status = "Selected tracking mode is unsupported for this printer.";
                }
            }
        }

        cupsFreeOptions(num_form, form);
    }

    // Re-read after potential changes
    papplPrinterGetDriverData(printer, &ddata);

    // Render page
    papplClientHTMLPrinterHeader(client, printer, "Media Setup", 0, NULL, NULL);

    if (status)
        papplClientHTMLPrintf(client, "<div class=\"banner\">%s</div>\n", status);

    papplClientHTMLStartForm(client, papplClientGetURI(client), false);

    papplClientHTMLPuts(client,
        "          <table class=\"form\">\n"
        "            <tbody>\n"
        "              <tr><th>Loaded Media:</th><td><select name=\"media-size\" "
        "onChange=\"document.getElementById('custom-fields').style.display="
        "this.value=='custom'?'block':'none';\">\n"
        "                <option value=\"custom\">New Custom Size</option>\n");

    // List known media sizes
    for (int i = 0; i < ddata.num_media && ddata.media[i]; i++) {
        const char *sel = "";
        char label[128];
        if (!strcmp(ddata.media[i], ddata.media_ready[0].size_name))
            sel = " selected";
        media_format_option_label(ddata.media[i], label, sizeof(label));
        papplClientHTMLPrintf(client,
            "                <option value=\"%s\"%s>%s</option>\n",
            ddata.media[i], sel, label);
    }

    papplClientHTMLPuts(client,
        "              </select><div class=\"form-help\">"
        "Entries shown as \"<width> mm continuous roll\" are endless stock "
        "(length is driven by the print job).</div></td></tr>\n");

    // Custom size fields
    bool show_custom = (!ddata.media_ready[0].size_name[0]);
    papplClientHTMLPrintf(client,
        "              <tr id=\"custom-fields\" style=\"display:%s;\">"
        "<th>Custom Size:</th><td>"
        "<input type=\"number\" name=\"custom-width\" min=\"10\" max=\"120\" "
        "step=\"0.1\" placeholder=\"Width\"> x "
        "<input type=\"number\" name=\"custom-length\" min=\"0\" max=\"2000\" "
        "step=\"0.1\" placeholder=\"Length (0=continuous)\"> "
        "<select name=\"custom-units\">"
        "<option value=\"mm\" selected>mm</option>"
        "<option value=\"in\">in</option>"
        "</select></td></tr>\n",
        show_custom ? "table-row" : "none");

    // Tracking
    papplClientHTMLPuts(client,
        "              <tr><th>Tracking:</th><td><select name=\"media-tracking\">\n");

    static const struct { const char *value; const char *label; int tracking; } trackings[] = {
        { "continuous", "Continuous", PAPPL_MEDIA_TRACKING_CONTINUOUS },
        { "gap",        "Gap/Die-cut", PAPPL_MEDIA_TRACKING_GAP },
        { "mark",       "Black Mark", PAPPL_MEDIA_TRACKING_MARK },
    };
    for (int i = 0; i < 3; i++) {
        const char *sel = (ddata.media_ready[0].tracking == trackings[i].tracking) ? " selected" : "";
        papplClientHTMLPrintf(client,
            "                <option value=\"%s\"%s>%s</option>\n",
            trackings[i].value, sel, trackings[i].label);
    }

    char current_label[128];
    media_format_option_label(ddata.media_ready[0].size_name, current_label, sizeof(current_label));

    papplClientHTMLPrintf(client,
        "              </select></td></tr>\n"
        "              <tr><th>Current:</th><td>%s (%dx%d hundredths-mm)</td></tr>\n"
        "              <tr><th></th><td><input type=\"submit\" value=\"Save Changes\">"
        "</td></tr>\n"
        "            </tbody>\n"
        "          </table>\n"
        "        </form>\n",
        current_label[0] ? current_label : ddata.media_ready[0].size_name,
        ddata.media_ready[0].size_width,
        ddata.media_ready[0].size_length);

    papplClientHTMLPrinterFooter(client);
    return true;
}

// ---------------------------------------------------------------------------
// Printer create callback — register media page + load persisted media
// ---------------------------------------------------------------------------

void media_printer_created(pappl_printer_t *printer, void *data) {
    (void)data;

    // Register the custom media setup page for this printer.
    char resource[256];
    if (media_get_web_resource(printer, "media-setup", resource, sizeof(resource))) {
        papplSystemAddResourceCallback(
            papplPrinterGetSystem(printer),
            resource,
            "text/html",
            (pappl_resource_cb_t)media_page_cb,
            printer);
        papplPrinterRemoveLink(printer, "Media Setup");
        papplPrinterAddLink(
            printer,
            "Media Setup",
            resource,
            PAPPL_LOPTIONS_NAVIGATION | PAPPL_LOPTIONS_CONFIGURATION);
    }

    // Load persisted custom media
    char saved_name[128];
    if (media_load(printer, saved_name, sizeof(saved_name)))
        media_apply(printer, saved_name);

    // Keep persisted state coherent for 0mm (continuous roll) media.
    media_normalize_ready_state(printer);
}
