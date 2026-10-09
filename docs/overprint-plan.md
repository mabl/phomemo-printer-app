# Plan: overprint label profiles

Status: **in progress**. This plan is updated as work packages land; the
status table at the end is authoritative.

## 1. Goal

Let designers lay out a label on a canvas larger than the physical label,
so that backgrounds and artwork reach past the label's edges and the label
is covered even when it sits slightly off position in the printer.

The first and only initially supported profile:

| Property | Value |
| --- | --- |
| Model | Phomemo M220 (576-dot / 72-byte head, 203 dpi) |
| Physical stock | 40 x 30 mm gap labels |
| Bleed in the design | 2 mm on every side |
| Design canvas | **44 x 34 mm**, `om_40x30mm-overprint-2mm_44x34mm` |
| Label inside the canvas | x = 2-42 mm, y = 2-32 mm |
| Scaling | none: the canvas is printed 1:1 |

What reaches paper depends on hardware limits (section 2): the **right**
bleed never prints on the M220, the **left** bleed prints from the first
release, and **bottom** and **top** bleed follow only after hardware
validation (H2, H3). With the default policy only the left 2 mm bleed reaches
paper; the goal of covering an off-position label is met on the left edge
only until `trailing` (bottom) and `full` (top) are validated.

The design application prints through **CUPS** (a driverless IPP Everywhere
queue made by `phomemo-printer-app register-cups`), so that path is the one
that must work and be tested end to end.

Non-goals for now: other models or stock sizes (the profile table takes them
later), automatic bleed synthesis (edge extension, zoom-crop), raw
(`application/vnd.phomemo-raw`) jobs, and sideways (D30-class) media.

## 2. Geometry

Dots are 203 dpi. In integers, for `h` hundredths of a millimetre:

```text
dots_floor(h) = h * 203 / 2540
dots_round(h) = (h * 203 + 1270) / 2540
```

### 2.1 Across the head (x)

An ordinary 40 x 30 mm job sends a 40-byte bitmap with `LEFT_MARGIN = 32`,
which prints correctly (`docs/m220-positioning.md`, "Established baseline").
The ordinary raster is 320 dots wide when CUPS rasterizes (it rounds) and
319 when PAPPL's image filter does (it truncates); both pack into 40 bytes,
so the label's first column is head dot 256 either way. Earlier
reverse-engineering found that the label is held against the far end of the
head, so the label's right edge is at or near the head's last dot (575) and
**the right bleed cannot print**. H0 measures the actual edges.

The design convention stays symmetric (44 mm canvas); the driver discards
the right bleed. The anchor is the **label's left edge**, which must land on
the same head dot as an ordinary job's first column:

```text
stock_bytes = ceil(dots_floor(stock_width) / 8)   40 for 40 mm (319.69 dots)
anchor      = (head_bytes - stock_bytes) * 8       head dot 256
x0          = dots_round(bleed_left)               canvas column 16
b           = ceil(x0 / 8)                         left-bleed bytes: 2
pad         = 8 * b - x0                           white columns first: 0
margin      = head_bytes - stock_bytes - b         LEFT_MARGIN: 30
out_width   = (stock_bytes + b) * 8                bitmap width: 336 dots
output column o  <->  canvas column  o - 8 * b + x0
```

Canvas column `x0` lands on head dot `margin * 8 + 8 * b = anchor`; with
these numbers output column `o` is canvas column `o`, canvas columns 0-335
are kept and column 335 lands on head dot 575. Columns mapping past
`out_width` (the right bleed) are dropped; output columns that map before
the canvas or past its width are **white in the raster's own encoding**
(0 for `K`, 255 for `W`/`SW`, as PAPPL pads in `job-process.c`). If `margin`
would be negative, `b` is reduced until it is 0 (not reachable here). With
an odd `x0` (e.g. 15) `pad` is 1 and the last kept canvas column is 334.
Rasters 351 and 352 wide both cover column 335.

Because the bitmap's right edge is the head's last dot, the existing
`LeftMargin::for_width(head_bytes, stride)` yields `margin` unchanged.

### 2.2 Along the feed (y)

```text
y0 = dots_round(bleed_top)                           canvas row 16
y1 = min(dots_round(bleed_top + stock_length), H)    canvas row 256
y2 = min(dots_round(bleed_top + stock_length + bleed_bottom), H)
                                                     canvas row 272
```

A page with `y0 >= y1` is printed as an ordinary page. In gap mode the
printer starts each raster where it detects the label, so rows before `y0`
would print *on* the label and push the artwork down. Which rows are sent is
the **vertical policy**:

| Policy | Rows sent | Hardware status |
| --- | --- | --- |
| `clip` | `y0 .. y1` (240 rows) | Same length as an ordinary job. Default. |
| `trailing` | `y0 .. y2`, i.e. `y0 .. min(H, y1 + bottom bleed)` (256 rows) | Bottom bleed prints into the gap; a page longer than the canvas sends no more. Unvalidated (H2). |
| `full` | `0 .. H` plus a positioning prelude | Unvalidated (H3); not offered before. |

`full` needs the printer to start about 2 mm before the label. The only
demonstrated mechanism is `BACK_PAPER` (about 3.2 mm earlier) with
continuous-mode `ESC d` (about 0.5 mm/unit, so compensation is no finer than
that), and the first raster's origin varied by about 0.8 mm between
continuous-mode passes (`docs/m220-positioning.md`). Today every page's
preamble ends with `ESC @`; a prelude must come after that reset, with no
further reset before the raster. Hence a separate, gated work package.

## 3. Design decisions

**D1 - Canvas sizes are separate from stock.** The catalog
(`phomemo-protocol/data/media_catalog.json`) stays a list of physical stock.
Profiles live in a small hand-written table in `phomemo-pappl` and are
advertised in addition to the catalog's sizes. Loaded ("ready") media should
always be physical stock, but PAPPL's own Media page
(`printer-webif.c`, lists every non-`custom_`/`roll_` name) and IPP
Set-Printer-Attributes can still load a canvas, and PAPPL then also makes it
the default media. Therefore: resolution treats a ready canvas as its
profile's stock being loaded (and logs a warning), the app's Media Setup page
does not offer canvases and shows a ready canvas as "<canvas> (load
<stock> instead)", and a Media Setup POST naming a canvas is refused.

**D2 - The canvas has a real, self-describing PWG name** whose dimension
part is the canvas size, so every client sees a 44 x 34 mm page. Verified
with libcups: `pwgMediaForPWG("om_40x30mm-overprint-2mm_44x34mm")` gives
4400 x 3400. The name segment must not contain `_`. CUPS' generated PPD
names it `44x34mm.Borderless` (PAPPL's margins are 0).

**D3 - Resolution survives CUPS dropping the name.** CUPS' driverless path
sends `media-col` with dimensions only; PAPPL renames that size with
`pwgMediaForSize` (`custom_44x34mm_44x34mm`; 43.99 x 34.01 mm gives
`custom_43.99x34.01mm_...`). Without any `media-col`, `options->media` is the
printer's default, i.e. the ready stock. "Stock loaded" means: the ready
media is a label (length > 0) within 0.5 mm of the stock size, or is a canvas
of this profile (D1). A job uses a profile, on the profile's model, when:

1. its media name equals the canvas name (ASCII case-insensitive), or
2. its media size is within **0.5 mm** (CUPS' own `_PWG_EPSILON`) of the
   canvas in both axes and the stock is loaded, or
3. its raster header's page size is within 0.5 mm of the canvas and the
   stock is loaded. The page size is `cupsPageSize` (points) if both values
   are positive, else the integer `PageSize` (points) if both are positive,
   else `cupsWidth`/`cupsHeight` at `HWResolution` (pixels / dpi * 72).
   PWG raster carries no `cupsPageSize`: libcups reads it as 0 x 0 and
   sets only `PageSize` (124 x 96 for 44 x 34 mm, i.e. 43.74 x 33.87 mm,
   inside the tolerance); URF gives 124.49 x 96.12 and CUPS raster
   124.72 x 96.38.

A known page size that is not the canvas (outside 0.5 mm) makes the job
**ordinary**, even under rule 1 and before the stock is looked at: the
raster is what is printed, so naming the canvas with a 40 x 30 mm page and
other stock loaded is an ordinary job, not an error (a warning is logged
when the job media named the canvas, or measured it with the stock loaded;
otherwise a debug note). Otherwise, if 1 applies
but the stock is not loaded, the job **fails** with a clear message: there
is no correct geometry for other stock. If the header page is the canvas
but the job media is another size, the header wins: logged at info level
when the job media is the ready media (the job named none and inherited it,
CUPS' driverless path without `media-col`), as a warning when the job named
another size. A rotated 34 x 44 mm page does not match (logged at debug
level). Anything else is an ordinary job, exactly as today.

**D4 - No ready-media aliases.** PAPPL validates job media against
`media-supported`/`media-size-supported` only and holds no job for
`media-needed` (`printer-ipp.c`), so a canvas job prints without an alias.
Aliases would not persist in PAPPL 1.4 either (`system-loadsave.c` saves
`num_source` ready entries).

**D5 - The label anchor, not the canvas, is aligned** (section 2.1). The
right bleed is clipped; the whole canvas is never right-aligned.

**D6 - Tracking follows the loaded stock.** For a profile job the tracking is
`media_ready[0].tracking` (the stock's), whatever the job says. (On the CUPS
path, jobs carry no `media-tracking` and already inherit it.)

**D7 - The vertical policy is a vendor option settable as a printer
default**, `phomemo-overprint-vertical`, keywords `clip` (default) and
`trailing`; `full` only after H3. CUPS' generated PPD forwards no vendor
options, and PAPPL 1.4.12 looks up vendor defaults only in the job's
attributes, so a "Printing Defaults" value is stored but never used. The
driver therefore reads the job's value and, if absent, the printer's
`phomemo-overprint-vertical-default` from `papplPrinterGetDriverAttributes`
(a copy; `ippDelete` it). This is how a CUPS user selects the policy. The
same fallback is applied to `phomemo-dither` and `phomemo-compression` in a
separate commit (WP3b), since they have the same defect today.

**D8 - Human-readable names** come from a strings catalog registered with
`papplSystemAddStringsData` for `en`. It merges into PAPPL's `en` strings
and cannot shadow them: built-ins load first and the first entry for a key
wins (`loc.c`). PAPPL keeps a pointer, so the data must be static memory
owned by Rust for the process lifetime. Keys: `media.<canvas name>`,
`phomemo-overprint-vertical`, and `phomemo-overprint-vertical.<keyword>`.
Effect: PAPPL's web UI and IPP clients that read `printer-strings-uri` show
"40 x 30 mm + 2 mm overprint"; CUPS-generated PPDs show "44 x 34 mm".
Registering a strings file also makes `printer-strings-languages-supported`
`en` only; accepted.

## 4. Architecture

```text
c/driver.c          driver_cb: canvas names in the media list, vendor option
                    supported/default; driver_rstartjob: pass the job context
                    (ready media, vendor defaults) to pm_job_start;
                    driver_options: job media name/size, header page size,
                    the job's vendor option.
c/media.c           Media Setup: canvases not offered/refused; ready canvas
                    shown as its stock; overprint designs listed.
c/main.c            system_cb: register the strings catalog.
phomemo-pappl/src/
  overprint.rs      NEW: profile table, resolution (D3), geometry (2.1/2.2),
                    strings catalog, small FFI helpers for c/media.c.
  models.rs         Model::overprint_profiles().
  defaults.rs       media list = catalog + canvases + custom range bounds.
  raster/ffi.rs     PmOptions / PmJobContext fields, Ops::read_options.
  raster/options.rs PrintOptions fields, vertical policy parsing/fallback.
  raster/page.rs    Layout gains a column mapping with white padding.
  raster/mod.rs     Job keeps the job context; start_page resolves the
                    profile; end_page uses the stock tracking.
docs/overprint.md   NEW user guide; docs/templates/*.svg design template.
```

## 5. Work packages

Process for every software work package: an implementer subagent does the
work; an adversarial-review subagent checks it against this plan and the
checklist (section 7); findings are fixed; `nix develop --command make
check` (and the Python tests where touched) passes; one commit. **Every
subagent runs every command with working directory
`/home/mabl/development/phomemo_printer_app/repo`** and uses absolute paths
for file tools. Subagents never print, never connect to the printer and
never commit. Hardware steps (H0-H3) are run by the coordinator with the
operator, who is asked before every print.

### WP1 - Plan (this document)

Commit: `docs: plan overprint label profiles`.

### WP2 - Profile model and geometry (pure Rust)

Files: `phomemo-pappl/src/overprint.rs` (new), `phomemo-pappl/src/lib.rs`,
`phomemo-pappl/src/models.rs`.

- `OverprintProfile`: model name, stock size (hundredths mm) and catalog
  name, bleed per side (left, top, right, bottom), canvas size and name
  (derived; checked against D2's rule), human-readable label.
- One profile: M220, 40 x 30 mm, 200 on every side.
- `Model::overprint_profiles()`: only models without sideways media, whose
  stock is in the model's catalog, and whose canvas `Model::accepts_media`.
- Resolution per D3 as a pure function of: model, job media name and size,
  header page size (points), ready media name and size and length. Result:
  ordinary, profile (with which rule matched and any notes), or error. A
  helper derives the page size from the header per D3's fallback order.
- `Geometry` per section 2 with the integer formulas; row ranges per
  policy; handles canvases narrower/shorter than expected without panics or
  out-of-range indexing.

Tests: section 2's numbers; odd `x0`; 351/352 x 271/272 rasters; each
resolution rule, tolerance edges (0.5 mm inside/outside), ready canvas,
custom-named 40 x 30 stock, other stock with case 1 (error), rotated page,
other models; the canvas name parses back to 44 x 34 mm; canvas accepted.

Commit: `pappl: add overprint profiles and their geometry`.

### WP3 - Raster path

Files: `phomemo-pappl/src/raster/{page.rs,mod.rs,options.rs,ffi.rs}`,
`c/driver.c` (header regenerated by `make`).

- `driver_rstartjob` copies `media_ready[0]` (name, width, length,
  tracking) and the printer's vendor defaults (`phomemo-overprint-vertical`
  for now) into a by-value `PmJobContext` passed to `pm_job_start` and kept
  in `Job` for all pages; read once per job.
- `PmOptions` gains the job's media name, width and length, the header's
  `cupsPageSize`, `PageSize`, `cupsWidth`/`cupsHeight` and `HWResolution`
  (D3's page-size fallback, `overprint::page_points`; `RasterHeader`
  already carries the width and height, not the resolution or page sizes),
  and the job's `phomemo-overprint-vertical`. Pointers are only into
  `options`, valid for the callback; no pointer into a stack copy.
- `PrintOptions::overprint_vertical()`: job value, else printer default,
  else `clip`; unknown values logged and treated as `clip`.
- `Layout` gains an explicit column mapping (source start, white padding,
  output width); `Page` pads with the encoding's white; memory stays
  bounded by the head width.
- `Job::start_page` resolves the profile, logs profile/rule/policy, fails
  the page on a resolution error; `end_page` uses the stock's tracking.
- Resolution guard: a page that resolves to a profile is printed as an
  ordinary page, with a warning that the design must be rasterized at the
  model's dpi at 100 %, if its `HWResolution` is set and is not the
  model's dpi in both axes, or its width differs from the canvas's width
  in dots (`dots_round(canvas width)`, 352 at 203 dpi) by more than 4
  dots. A shrunk-to-fit or other-resolution raster has no correct
  geometry; printing it as it is matches what it would have done before.
  The same happens, with a warning, when the raster does not reach the
  label (`Geometry::new` is `None`). The raster header is checked first,
  the policy is read only once a geometry exists, and the profile line is
  logged only once the page has started.
- Tracking fallback for profile pages: the ready media's tracking if it is
  a single known `pappl_media_tracking_t` flag, else the job's tracking,
  else gap (a profile's stock is labels).

Tests: synthetic canvas pages (352 x 272 `W`, 351 x 271 `K`, odd `x0`) with
marks at the label corners and in each bleed: marks land at the expected
bitmap columns/rows, right bleed absent, `LEFT_MARGIN` 30; ordinary 319- and
320-wide and profile 351- and 352-wide jobs put the label's first column on
head dot 256; `clip`/`trailing` row counts; policy fallback order; ordinary
jobs byte-identical to before (existing tests stay green).

Commit: `pappl: print overprint canvases anchored to the physical label`.

### WP3b - Printer-level vendor defaults for dither and compression

Apply D7's fallback to `phomemo-dither` and `phomemo-compression` via
`PmJobContext`. Tests for the fallback order.

Commit: `pappl: honour printer defaults for vendor options`.

### WP4 - Advertising, Media Setup and names

Files: `phomemo-pappl/src/{defaults.rs,overprint.rs}`, `c/driver.c`,
`c/media.c`, `c/main.c`.

- Media list: catalog sizes, then canvases, then the custom range bounds
  (PAPPL finds bounds by prefix anywhere; the order matters only to this
  repo's tests). Assert it fits `PM_MAX_MEDIA`. The canvas lies inside the
  custom range, so ranges and `continuous` detection are unchanged.
- `phomemo-overprint-vertical` registered as a vendor option with
  `-supported` (`clip`, `trailing`) and `-default` (`clip`).
- Media Setup per D1, plus a row listing the overprint designs available for
  the loaded stock (canvas size, where the label sits, policy).
- Strings catalog per D8, static, registered in `system_cb`.

Tests: Rust tests for media list contents/order and the strings catalog;
`make c-lint`; manual check against a dev server (H-setup below):
`ipptool`/`lpstat -l` show the canvas in `media-supported` and
`media-col-database`, the Media Setup and Printing Defaults pages render.

Commit: `pappl: advertise overprint canvases`.

### WP5 - Documentation and design template

Files: `docs/overprint.md` (new), `docs/templates/m220-40x30-overprint-2mm.svg`
(new, 44 x 34 mm, non-printing guides for the label and the clipped right
bleed), `README.md` (short section and link), this plan's status.

The guide covers: choosing the 44 x 34 mm page (`44x34mm.Borderless` in
CUPS) or the named size, 100 % / no fit-to-page, the label rectangle, that
the right 2 mm never prints on the M220, the vertical policies and how to set
the printer default, CUPS' name loss (D3), that an existing CUPS queue must
be recreated (`register-cups --replace`) to offer the size, and that PAPPL's
own PNG/JPEG path places images by `print-scaling` (1:1 only for a
351 x 271 image at 203 dpi; PAPPL has no PDF filter).

Commit: `docs: document overprint labels`.

### H-setup - Hardware test environment

The system service keeps the Bluetooth link open between jobs (status polls
from CUPS, print panels and the web UI; 30 s idle timeout), and the printer
takes one connection. For H0-H3 the operator stops it
(`sudo systemctl stop phomemo-printer-app`) and restarts it afterwards. A dev
server runs from the build directory on another port with its own state
file; `phomemo-printer-app register-cups --queue phomemo-dev --port <port>`
creates a CUPS test queue whose PPD must list `44x34mm.Borderless`.

### H0 - Edge probe (1 label)

Print a 72-byte-wide raster with 1-dot vertical ticks every 4 dots from dot
224 to 575 (longer every 16). Measure which ticks land on the label and on
the backing. Pass: the label's left edge within ±4 dots of 256, its right
edge within 4 dots of 575, dots 240-255 on the backing. Otherwise section 2
is revised before H1.

### H1 - Horizontal anchor and `clip` through CUPS (2-3 labels)

Through `lp -d phomemo-dev -o media=44x34mm.Borderless` (dimension-only
`media-col`, D3 case 2): one ordinary 40 x 30 mm print of the same artwork as
reference, one profile print. Pass: the profile print's artwork is within
0.3 mm of the reference horizontally, the left bleed covers the label's left
edge, the next label is unaffected. Vertical offset is recorded, not judged.

### H2 - `trailing` (2 labels)

Printer default `phomemo-overprint-vertical=trailing`, two consecutive
profile jobs through CUPS. Pass: the bottom bleed covers the trailing edge,
no label is skipped, the second label registers like an ordinary job.
Otherwise `trailing` is not recommended in the docs and the finding recorded.

### H3 - Leading-edge positioning for `full` (gated, <= 4 labels)

First with diagnostics like `scripts/m220_feed_probe.py`, then in the driver:
preamble ending in `ESC @` (gap), `BACK_PAPER`, continuous mode, `ESC d n`,
gap mode, raster, with no reset in between. Pass: the top bleed covers the
leading edge on 3 consecutive labels and the artwork stays within 0.5 mm of
an ordinary job. Only then a WP adds `full` (prelude in
`phomemo-protocol::job`; cancellation restores gap mode). Never send
`VERIFY_PAPER` with paper loaded.

Results of H0-H3 are recorded in `docs/m220-positioning.md`.

### Deployment

The user's own CUPS queue points at the NixOS service, so the change reaches
it only after a NixOS rebuild/switch, followed by
`phomemo-printer-app register-cups --queue phomemo --port <port> --replace`
(without `--replace` the queue keeps its old PPD). This is the operator's
step; the docs describe it.

## 6. Risks and open questions

- Some designer applications may not offer custom page sizes through the
  driverless PPD; a user-defined 44 x 34 mm size still resolves (D3 case 2).
- Client-side fit-to-page shrinks the canvas and defeats the anchor;
  documented, not detectable by the driver.
- H0 may show the label is not exactly at the head's end; section 2 then
  changes (e.g. a measured anchor per profile).
- Gap-mode registration repeatability has never been measured; manual
  readings are ±0.1 mm.

## 7. Adversarial review checklist (every commit)

- Ordinary (non-profile) jobs produce byte-identical printer output.
- Geometry: off-by-one at `x0`, `y0`, `y1`, byte padding, white value per
  colour space, rasters smaller or larger than the canvas, overflow,
  unbounded allocation.
- FFI: ownership and lifetime of every pointer, NUL termination, C and Rust
  struct layouts agree (header regenerated by `make`).
- PAPPL: media list contents and limits, vendor option advertisement and
  fallback, persistence, ready canvas handling.
- Tests assert behaviour, not implementation; docs match code and evidence.
- `nix develop --command make check` passes.

## 8. Status

| Item | Status | Commit |
| --- | --- | --- |
| WP1 Plan | done | `docs: plan overprint label profiles` |
| WP2 Profile model and geometry | done | `pappl: add overprint profiles and their geometry` |
| WP3 Raster path | done | `pappl: print overprint canvases anchored to the physical label` |
| WP3b Printer defaults for vendor options | pending | |
| WP4 Advertising, Media Setup, names | pending | |
| WP5 Documentation and template | pending | |
| H0 Edge probe | pending | |
| H1 Horizontal anchor and `clip` | pending | |
| H2 `trailing` | pending | |
| H3 Leading-edge positioning, `full` | pending | |
