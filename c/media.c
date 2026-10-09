//
// Phomemo Printer Application — media setup web page.
//
// Each printer has one media source, "main-roll". Its "Media Setup" page
// selects the size loaded, one of the model's or a custom one, and how the
// printer tracks it. The page sets PAPPL's ready media, which PAPPL saves in
// its state file (media-col-ready), so the choice survives a restart.
//
// Overprint canvases (docs/overprint-plan.md, D1) are design sizes, listed
// in the driver's media list for clients but not media to load: the page
// does not offer them, refuses them, shows a loaded one as its stock, and
// lists the designs the loaded stock takes.
//

#include <stdio.h>
#include <string.h>
#include "phomemo.h"

// The media-size value of the custom size fields.
#define MEDIA_SIZE_CUSTOM "custom"

// The custom sizes the driver takes, in hundredths of a millimetre: the
// bounds its media list names (media_is_range_bound). PAPPL accepts ready
// media within the range of the whole list (validate_ready in PAPPL's
// printer-driver.c), which includes these bounds and, if the list has a
// roll, a length of 0.
typedef struct {
    int  min_width, max_width;
    int  min_length, max_length;
    bool continuous;              // whether a custom roll, 0 long, fits
} media_range_t;

// A unit custom sizes are entered in.
typedef struct {
    const char *name;             // as in PWG media names
    int         hundredths;       // hundredths of a millimetre per unit
    int         decimals;         // decimal places shown
} media_unit_t;

static const media_unit_t media_units[] = {
    { "mm", 100,  2 },
    { "in", 2540, 3 },
};

#define MEDIA_NUM_UNITS (sizeof(media_units) / sizeof(media_units[0]))

// How media_format_in_unit rounds.
typedef enum {
    MEDIA_ROUND_DOWN,
    MEDIA_ROUND_NEAREST,
    MEDIA_ROUND_UP,
} media_rounding_t;

// The tracking modes the page offers, where the printer supports them.
static const struct {
    const char            *keyword;
    const char            *label;
    pappl_media_tracking_t tracking;
} media_trackings[] = {
    { "continuous", "Continuous",  PAPPL_MEDIA_TRACKING_CONTINUOUS },
    { "gap",        "Gap/Die-cut", PAPPL_MEDIA_TRACKING_GAP },
    { "mark",       "Black Mark",  PAPPL_MEDIA_TRACKING_MARK },
};

#define MEDIA_NUM_TRACKINGS (sizeof(media_trackings) / sizeof(media_trackings[0]))

// ---------------------------------------------------------------------------
// Media sizes
// ---------------------------------------------------------------------------

// Whether `size_name` is a bound of a driver's custom size range rather than
// a size, as PAPPL tells them apart (make_attrs in printer-driver.c).
static bool media_is_range_bound(const char *size_name) {
    return !strncmp(size_name, "roll_min_", 9) || !strncmp(size_name, "roll_max_", 9) ||
           !strncmp(size_name, "custom_min_", 11) || !strncmp(size_name, "custom_max_", 11);
}

// Whether the driver lists size `size_name`.
static bool media_is_listed(const pappl_pr_driver_data_t *data, const char *size_name) {
    for (int i = 0; i < data->num_media; i++) {
        if (!media_is_range_bound(data->media[i]) && !strcmp(data->media[i], size_name))
            return true;
    }
    return false;
}

// Find the driver's custom size range; false if it names none.
static bool media_custom_range(const pappl_pr_driver_data_t *data, media_range_t *range) {
    bool have_min = false, have_max = false;

    range->continuous = false;
    for (int i = 0; i < data->num_media; i++) {
        const char *size_name = data->media[i];
        // For a name it does not know, pwgMediaForPWG fills in and returns
        // one buffer, which the next call overwrites.
        const pwg_media_t *pwg = pwgMediaForPWG(size_name);

        if (!pwg) {
            continue;
        } else if (!media_is_range_bound(size_name)) {
            range->continuous = range->continuous || pwg->length == 0;
        } else if (strstr(size_name, "_min_")) {
            range->min_width  = pwg->width;
            range->min_length = pwg->length;
            have_min = true;
        } else {
            range->max_width  = pwg->width;
            range->max_length = pwg->length;
            have_max = true;
        }
    }

    return have_min && have_max;
}

// The unit a PWG media name gives its size in.
static const media_unit_t *media_name_unit(const char *size_name) {
    size_t length = strlen(size_name);
    for (size_t i = 0; i < MEDIA_NUM_UNITS; i++) {
        if (length > 2 && !strcmp(size_name + length - 2, media_units[i].name))
            return &media_units[i];
    }
    return &media_units[0];
}

static const media_unit_t *media_find_unit(const char *name) {
    for (size_t i = 0; name && i < MEDIA_NUM_UNITS; i++) {
        if (!strcmp(name, media_units[i].name))
            return &media_units[i];
    }
    return NULL;
}

// Format `hundredths` of a millimetre in `unit`, rounded as `rounding` says
// to the unit's decimal places, without trailing zeros.
static void media_format_in_unit(int hundredths, const media_unit_t *unit,
                                 media_rounding_t rounding, char *buffer, size_t bufsize) {
    long long scale = 1;
    for (int i = 0; i < unit->decimals; i++)
        scale *= 10;

    long long numerator = (long long)hundredths * scale;
    long long divisor = unit->hundredths;
    if (rounding == MEDIA_ROUND_NEAREST)
        numerator += divisor / 2;
    else if (rounding == MEDIA_ROUND_UP)
        numerator += divisor - 1;
    long long scaled = numerator / divisor;  // hundredths >= 0

    int length = snprintf(buffer, bufsize, "%lld.%0*lld",
                          scaled / scale, unit->decimals, scaled % scale);
    if (length < 0 || (size_t)length >= bufsize)
        return;
    while (buffer[length - 1] == '0')
        buffer[--length] = '\0';
    if (buffer[length - 1] == '.')
        buffer[length - 1] = '\0';
}

static void media_format_mm(int hundredths, char *buffer, size_t bufsize) {
    media_format_in_unit(hundredths, &media_units[0], MEDIA_ROUND_NEAREST, buffer, bufsize);
}

// Describe media `size_name`, e.g. "40 x 30 mm label".
static void media_format_label(const char *size_name, char *buffer, size_t bufsize) {
    const pwg_media_t *pwg = pwgMediaForPWG(size_name);
    if (!pwg) {
        papplCopyString(buffer, size_name, bufsize);
        return;
    }

    char width[32], length[32];
    media_format_mm(pwg->width, width, sizeof(width));
    media_format_mm(pwg->length, length, sizeof(length));

    if (pwg->length == 0)
        snprintf(buffer, bufsize, "%s mm continuous roll", width);
    else
        snprintf(buffer, bufsize, "%s x %s mm label", width, length);
}

// Find the overprint canvas `size_name` names; false if it is not one.
static bool media_find_canvas(const pappl_pr_driver_data_t *data, const char *size_name,
                              PmOverprintInfo *canvas) {
    int index = pm_overprint_find(data->extension, size_name);
    return index >= 0 && pm_overprint_get(data->extension, (unsigned)index, canvas);
}

// ---------------------------------------------------------------------------
// Form handling
// ---------------------------------------------------------------------------

// Parse a plain decimal number, as a number field submits it, into
// millionths: digits with an optional fraction after a '.', whatever the
// locale, and no sign, spaces or exponent. Up to 7 digits before the point
// and 6 after it, so that converting the result cannot overflow.
static bool media_parse_decimal(const char *value, long long *millionths) {
    long long whole = 0, fraction = 0, place = 1000000;
    int digits = 0;
    const char *p = value;

    for (; *p >= '0' && *p <= '9'; p++, digits++) {
        if (digits == 7)
            return false;
        whole = whole * 10 + (*p - '0');
    }

    if (*p == '.') {
        for (p++; *p >= '0' && *p <= '9'; p++, digits++) {
            if (place == 1)
                return false;
            place /= 10;
            fraction += (*p - '0') * place;
        }
    }

    if (*p || digits == 0)
        return false;

    *millionths = whole * 1000000 + fraction;
    return true;
}

// Convert a custom dimension in `unit` to hundredths of a millimetre,
// rounded to the nearest; false unless it is a number from `min` to `max`.
static bool media_parse_dimension(const char *value, const media_unit_t *unit,
                                  int min, int max, int *hundredths) {
    long long millionths;
    if (!value || !media_parse_decimal(value, &millionths))
        return false;

    long long result = (millionths * unit->hundredths + 500000) / 1000000;
    if (result < min || result > max)
        return false;

    *hundredths = (int)result;
    return true;
}

// Load media `size_name` into `ready`, with the tracking that suits it, if
// it is a different size; NULL, or what is wrong with it.
static const char *media_load_size(const pappl_pr_driver_data_t *data,
                                   const char *size_name, pappl_media_col_t *ready) {
    if (!strcmp(size_name, ready->size_name))
        return NULL;

    const pwg_media_t *pwg = pwgMediaForPWG(size_name);
    if (!pwg)
        return "Unknown media size.";

    papplCopyString(ready->size_name, size_name, sizeof(ready->size_name));
    ready->size_width  = pwg->width;
    ready->size_length = pwg->length;
    papplCopyString(ready->type, pm_media_type(pwg->length), sizeof(ready->type));
    ready->tracking    = pm_media_tracking(data->extension, size_name, pwg->length);
    return NULL;
}

// Load the custom size the form describes into `ready`; NULL, or what is
// wrong with the size.
static const char *media_load_custom_size(const pappl_pr_driver_data_t *data,
                                          int num_form, cups_option_t *form,
                                          pappl_media_col_t *ready) {
    static const char invalid[] = "Enter a custom width and length within the ranges shown.";

    media_range_t range;
    if (!media_custom_range(data, &range))
        return "This printer takes no custom sizes.";

    const media_unit_t *unit = media_find_unit(cupsGetOption("custom-units", num_form, form));
    int width, length;
    if (!unit ||
        !media_parse_dimension(cupsGetOption("custom-width", num_form, form), unit,
                               range.min_width, range.max_width, &width) ||
        !media_parse_dimension(cupsGetOption("custom-length", num_form, form), unit,
                               0, range.max_length, &length) ||
        (length == 0 ? !range.continuous : length < range.min_length))
        return invalid;

    // The size loaded, perhaps in the other unit: keep its name and tracking.
    // Not for a loaded canvas, which is no media to keep (D1).
    if (width == ready->size_width && length == ready->size_length &&
        !pm_overprint_is_canvas(data->extension, ready->size_name))
        return NULL;

    if (!pm_media_fits(data->extension, width, length))
        return "The custom size is too wide for this printer.";

    char size_name[sizeof(ready->size_name)];
    if (!pwgFormatSizeName(size_name, sizeof(size_name), "custom", NULL, width, length,
                           unit->name))
        return "Invalid custom size.";

    return media_load_size(data, size_name, ready);
}

// Load the size the form selects into `ready`; NULL, or what is wrong with
// the selection, perhaps formatted into `buffer`. An overprint canvas is a
// design size, not media to load (D1).
static const char *media_select_size(const pappl_pr_driver_data_t *data,
                                     const char *size_name,
                                     int num_form, cups_option_t *form,
                                     pappl_media_col_t *ready,
                                     char *buffer, size_t bufsize) {
    PmOverprintInfo canvas;

    if (!strcmp(size_name, MEDIA_SIZE_CUSTOM))
        return media_load_custom_size(data, num_form, form, ready);
    if (media_find_canvas(data, size_name, &canvas)) {
        char stock[128];
        media_format_label(canvas.stock_name, stock, sizeof(stock));
        snprintf(buffer, bufsize, "%s is a design size, not loaded media; load the %s.",
                 canvas.label, stock);
        return buffer;
    }
    if (!media_is_listed(data, size_name))
        return "Unknown media size.";
    return media_load_size(data, size_name, ready);
}

// Apply the tracking the form selects to `ready`, if the user changed it;
// NULL, or what is wrong with the selection. The form shows the tracking in
// effect, so an unchanged one leaves the tracking that suits a new size.
static const char *media_select_tracking(const pappl_pr_driver_data_t *data,
                                         const char *keyword,
                                         pappl_media_col_t *ready) {
    for (size_t i = 0; i < MEDIA_NUM_TRACKINGS; i++) {
        pappl_media_tracking_t tracking = media_trackings[i].tracking;
        if (strcmp(keyword, media_trackings[i].keyword))
            continue;

        if (tracking == data->media_ready[0].tracking)
            return NULL;
        if (!(data->tracking_supported & tracking))
            return "Selected tracking mode is unsupported for this printer.";

        ready->tracking = tracking;
        return NULL;
    }

    return "Unknown tracking mode.";
}

// Set the ready media to what the form selects; returns the status to show,
// perhaps formatted into `buffer`.
static const char *media_update(pappl_printer_t *printer, int num_form, cups_option_t *form,
                                char *buffer, size_t bufsize) {
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    pappl_media_col_t ready = data.media_ready[0];
    const char *size_name = cupsGetOption("media-size", num_form, form);
    const char *tracking = cupsGetOption("media-tracking", num_form, form);
    const char *error = NULL;

    if (size_name)
        error = media_select_size(&data, size_name, num_form, form, &ready, buffer, bufsize);
    if (!error && tracking)
        error = media_select_tracking(&data, tracking, &ready);
    if (error)
        return error;

    // Also saves the state and makes the media the default (printer-driver.c).
    if (!papplPrinterSetReadyMedia(printer, 1, &ready))
        return "The printer does not take this media.";

    return "Changes saved.";
}

// Handle a form submission; returns the status to show, perhaps formatted
// into `buffer`.
static const char *media_post(pappl_client_t *client, pappl_printer_t *printer,
                              char *buffer, size_t bufsize) {
    cups_option_t *form = NULL;
    int num_form = papplClientGetForm(client, &form);
    const char *status;

    if (num_form == 0)
        status = "Invalid form data.";
    else if (!papplClientIsValidForm(client, num_form, form))
        status = "Invalid form submission.";
    else
        status = media_update(printer, num_form, form, buffer, bufsize);

    cupsFreeOptions(num_form, form);
    return status;
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

// The custom size fields, preset to `preset`, the size loaded (or for a
// loaded canvas, its stock), in the unit its name uses. The inputs' bounds take a size in either unit, as on PAPPL's own
// media page: each is rounded outwards, from the bound in inches for the
// minimum and in millimetres for the maximum. media_load_custom_size checks
// the size itself.
static void media_show_custom_size(pappl_client_t *client, const char *preset_name,
                                   int preset_width, int preset_length,
                                   const media_range_t *range, bool shown) {
    const media_unit_t *unit = media_name_unit(preset_name);
    const media_unit_t *mm = &media_units[0], *in = &media_units[1];
    int min_length = range->continuous ? 0 : range->min_length;
    char width[32], length[32];
    char input_min_width[32], input_max_width[32], input_min_length[32], input_max_length[32];
    char min_width[32], max_width[32], shortest[32], max_length[32];

    media_format_in_unit(preset_width, unit, MEDIA_ROUND_NEAREST, width, sizeof(width));
    media_format_in_unit(preset_length, unit, MEDIA_ROUND_NEAREST, length, sizeof(length));
    media_format_in_unit(range->min_width, in, MEDIA_ROUND_DOWN,
                         input_min_width, sizeof(input_min_width));
    media_format_in_unit(range->max_width, mm, MEDIA_ROUND_UP,
                         input_max_width, sizeof(input_max_width));
    media_format_in_unit(min_length, in, MEDIA_ROUND_DOWN,
                         input_min_length, sizeof(input_min_length));
    media_format_in_unit(range->max_length, mm, MEDIA_ROUND_UP,
                         input_max_length, sizeof(input_max_length));
    media_format_mm(range->min_width, min_width, sizeof(min_width));
    media_format_mm(range->max_width, max_width, sizeof(max_width));
    media_format_mm(range->min_length, shortest, sizeof(shortest));
    media_format_mm(range->max_length, max_length, sizeof(max_length));

    papplClientHTMLPrintf(client,
        "              <tr id=\"custom-fields\" style=\"display:%s;\">"
        "<th>Custom Size:</th><td>"
        "<input type=\"number\" name=\"custom-width\" min=\"%s\" max=\"%s\" "
        "step=\"any\" value=\"%s\" placeholder=\"Width\"> x "
        "<input type=\"number\" name=\"custom-length\" min=\"%s\" max=\"%s\" "
        "step=\"any\" value=\"%s\" placeholder=\"Length\"> "
        "<select name=\"custom-units\">",
        shown ? "table-row" : "none",
        input_min_width, input_max_width, width,
        input_min_length, input_max_length, length);

    for (size_t i = 0; i < MEDIA_NUM_UNITS; i++)
        papplClientHTMLPrintf(client, "<option value=\"%s\"%s>%s</option>",
                              media_units[i].name, &media_units[i] == unit ? " selected" : "",
                              media_units[i].name);

    papplClientHTMLPrintf(client,
        "</select><div class=\"form-help\">Width %s to %s mm, length %s to %s mm%s.</div>"
        "</td></tr>\n",
        min_width, max_width, shortest, max_length,
        range->continuous ? ", or 0 for a continuous roll" : "");
}

static void media_show_tracking(pappl_client_t *client, const pappl_pr_driver_data_t *data) {
    papplClientHTMLPuts(client,
        "              <tr><th>Tracking:</th><td><select name=\"media-tracking\">\n");

    for (size_t i = 0; i < MEDIA_NUM_TRACKINGS; i++) {
        pappl_media_tracking_t tracking = media_trackings[i].tracking;
        if (!(data->tracking_supported & tracking))
            continue;

        papplClientHTMLPrintf(client,
            "                <option value=\"%s\"%s>%s</option>\n",
            media_trackings[i].keyword,
            tracking == data->media_ready[0].tracking ? " selected" : "",
            media_trackings[i].label);
    }

    papplClientHTMLPuts(client, "              </select></td></tr>\n");
}

// The overprint designs the loaded stock takes, with where the label sits
// on each and the printer's vertical policy; nothing if there are none.
static void media_show_overprint(pappl_client_t *client, pappl_printer_t *printer,
                                 const pappl_pr_driver_data_t *data) {
    const pappl_media_col_t *ready = &data->media_ready[0];
    unsigned count = pm_overprint_count(data->extension);
    bool shown = false;

    for (unsigned i = 0; i < count; i++) {
        PmOverprintInfo canvas;
        if (!pm_overprint_get(data->extension, i, &canvas) ||
            !pm_overprint_stock_loaded(data->extension, i, ready->size_name,
                                       ready->size_width, ready->size_length))
            continue;

        if (!shown)
            papplClientHTMLPuts(client, "              <tr><th>Overprint designs:</th><td>");
        else
            papplClientHTMLPuts(client, "<br>");
        papplClientHTMLPrintf(client, "%s: %s.", canvas.label, canvas.summary);
        shown = true;
    }
    if (!shown)
        return;

    // The policy a job without its own value uses, decided as for a job.
    char value[64], path[1024];
    phomemo_vendor_default(printer, VENDOR_OVERPRINT_VERTICAL "-default", value, sizeof(value));
    const char *policy = pm_overprint_vertical_label(value);
    const char *option = pm_string_en(VENDOR_OVERPRINT_VERTICAL);
    papplPrinterGetPath(printer, "printing", path, sizeof(path));

    papplClientHTMLPrintf(client,
        "<div class=\"form-help\">Lay the design out on the design size at 100%%. "
        "%s: %s; change it under <a href=\"%s\">Printing Defaults</a>.</div>"
        "</td></tr>\n",
        option ? option : VENDOR_OVERPRINT_VERTICAL, policy, path);
}

static void media_show(pappl_client_t *client, pappl_printer_t *printer, const char *status) {
    pappl_pr_driver_data_t data;
    papplPrinterGetDriverData(printer, &data);

    const pappl_media_col_t *ready = &data.media_ready[0];
    media_range_t range;
    bool takes_custom = media_custom_range(&data, &range);
    // A loaded canvas is shown, and selected, as its stock (D1).
    PmOverprintInfo canvas;
    bool ready_canvas = media_find_canvas(&data, ready->size_name, &canvas);
    const char *selected = ready_canvas ? canvas.stock_name : ready->size_name;
    bool custom = !media_is_listed(&data, selected);
    char label[128];

    papplClientHTMLPrinterHeader(client, printer, "Media Setup", 0, NULL, NULL);
    if (status)
        papplClientHTMLPrintf(client, "<div class=\"banner\">%s</div>\n", status);

    papplClientHTMLStartForm(client, papplClientGetURI(client), false);
    papplClientHTMLPuts(client,
        "          <table class=\"form\">\n"
        "            <tbody>\n"
        "              <tr><th>Loaded Media:</th><td><select name=\"media-size\"");
    if (takes_custom)
        papplClientHTMLPrintf(client,
            " onChange=\"document.getElementById('custom-fields').style.display="
            "this.value=='%s'?'table-row':'none';\">\n"
            "                <option value=\"%s\"%s>New Custom Size</option>\n",
            MEDIA_SIZE_CUSTOM, MEDIA_SIZE_CUSTOM, custom ? " selected" : "");
    else
        papplClientHTMLPuts(client, ">\n");

    for (int i = 0; i < data.num_media; i++) {
        if (media_is_range_bound(data.media[i]) ||
            pm_overprint_is_canvas(data.extension, data.media[i]))
            continue;

        media_format_label(data.media[i], label, sizeof(label));
        papplClientHTMLPrintf(client,
            "                <option value=\"%s\"%s>%s</option>\n",
            data.media[i], strcmp(data.media[i], selected) ? "" : " selected", label);
    }

    papplClientHTMLPuts(client,
        "              </select><div class=\"form-help\">"
        "Entries shown as \"&lt;width&gt; mm continuous roll\" are endless stock "
        "(length is driven by the print job).</div></td></tr>\n");

    if (takes_custom)
        media_show_custom_size(client, selected,
                               ready_canvas ? canvas.stock_width : ready->size_width,
                               ready_canvas ? canvas.stock_length : ready->size_length,
                               &range, custom);
    media_show_tracking(client, &data);

    if (ready_canvas) {
        media_format_label(canvas.stock_name, label, sizeof(label));
        papplClientHTMLPrintf(client,
            "              <tr><th>Current:</th><td>%s (load the %s instead)</td></tr>\n",
            canvas.label, label);
    } else {
        media_format_label(ready->size_name, label, sizeof(label));
        papplClientHTMLPrintf(client,
            "              <tr><th>Current:</th><td>%s</td></tr>\n", label);
    }
    media_show_overprint(client, printer, &data);
    papplClientHTMLPuts(client,
        "              <tr><th></th><td><input type=\"submit\" value=\"Save Changes\">"
        "</td></tr>\n"
        "            </tbody>\n"
        "          </table>\n"
        "        </form>\n");

    papplClientHTMLPrinterFooter(client);
}

// The page's resource callback (pappl_resource_cb_t); `data` is the printer.
static bool media_page_cb(pappl_client_t *client, void *data) {
    pappl_printer_t *printer = data;
    const char *status = NULL;
    char buffer[256];

    // On failure, papplClientHTMLAuthorize has responded already.
    if (!papplClientHTMLAuthorize(client))
        return true;

    if (papplClientGetMethod(client) == HTTP_STATE_POST)
        status = media_post(client, printer, buffer, sizeof(buffer));

    media_show(client, printer, status);
    return true;
}

// ---------------------------------------------------------------------------
// Printer creation
// ---------------------------------------------------------------------------

// PAPPL calls this once for each printer it creates, including those it
// loads from its state file (papplPrinterCreate in printer.c), and removes
// the page along with the printer's other resources when it is deleted.
void phomemo_media_create_cb(pappl_printer_t *printer, void *data) {
    (void)data;

    char path[1024];
    if (!papplPrinterGetPath(printer, "media-setup", path, sizeof(path)))
        return;

    papplLogPrinter(printer, PAPPL_LOGLEVEL_DEBUG, "Adding media setup page '%s'.", path);
    papplSystemAddResourceCallback(papplPrinterGetSystem(printer), path, "text/html",
                                   media_page_cb, printer);
    papplPrinterAddLink(printer, "Media Setup", path,
                        PAPPL_LOPTIONS_NAVIGATION | PAPPL_LOPTIONS_CONFIGURATION);
}
