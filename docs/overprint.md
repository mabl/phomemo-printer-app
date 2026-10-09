# Overprint labels

An overprint design is laid out on a page larger than the label: the
label plus a bleed on every side. Backgrounds and artwork that run into the
bleed still cover the label's edge when the label sits slightly off
position in the printer. The driver prints the design 1:1, anchored to the
label rather than to the page, and drops the bleed it does not print.

**Status:** validated on an M220 (firmware 3.0.1) on 2026-10-09: an edge
probe sent directly, then H1-H2 through CUPS (H0-H2 in the
[plan](overprint-plan.md#8-status); details in
[m220-positioning.md](m220-positioning.md#overprint-validation-2026-10-09)):

- The label spans about head dots 256-575, ending at or within a couple of
  dots of the head's last dot: the right bleed cannot print.
- A 44 x 34 mm design lands within about 0.3 mm (the criterion's limit;
  read by eye) of an ordinary 40 x 30 mm print across the head, and its
  left bleed covers the label's left edge.
- With `trailing`, the bottom bleed covers the label's bottom edge, and on
  two consecutive labels the frames printed as designed, none skipped,
  with nothing spilled onto the next label.

This was one session: two labels for `trailing`, positions read by eye.
Registration along the feed depends on the printer's gap detection, and
the top bleed is still not printed.

## The M220 40 x 30 mm + 2 mm design

There is one design size, for the M220 (the `phomemo_m220` driver) with
40 x 30 mm gap labels loaded:

| Property | Value |
| --- | --- |
| Design page | 44 x 34 mm |
| PWG name | `om_40x30mm-overprint-2mm_44x34mm` |
| Shown as | "40 x 30 mm + 2 mm overprint" (web interface, IPP clients that read `printer-strings-uri`) |
| CUPS page size | `44x34mm.Borderless` |
| Label on the page | x = 2-42 mm, y = 2-32 mm |
| Bleed | 2 mm on every side |
| Resolution | 203 dpi, no scaling |

The page's x axis runs across the print head, its y axis along the feed:
the top of the page leaves the printer first.

```text
x (mm)  0    2                                    42   44
   y 0  +-----------------------------------------+----+
        |  top bleed: not printed                 |////|
   y 2  |    +------------------------------------+////|
        |    |                                    |////|
        |    |                                    |////|  right bleed:
        |    |          label, 40 x 30 mm         |////|  not printed
        |    |                                    |////|
        |    |                                    |////|
  y 32  |    +------------------------------------+////|
        |  bottom bleed: only with trailing       |////|
  y 34  +-----------------------------------------+----+
          ^
          left bleed: printed
```

## What prints

- **Right bleed (x = 42-44 mm): never.** The label is held against the far
  end of the head, so the head ends at the label's right edge (measured:
  the label ends at or within a couple of dots of the head's last dot),
  and the driver drops these columns.
- **Left bleed (x = 0-2 mm): always, beside the rows sent.** It prints
  beside the label's left edge: off the sticker when the label is in
  place, and onto the label when it sits further left. The backing liner
  is not thermal paper, so bleed that misses the sticker leaves no mark.
- **Top bleed (y = 0-2 mm): never, for now.** In gap mode the printer
  starts each print where it detects the label, so these rows would print
  on the label and push the design down. Printing them needs a positioning
  sequence that has not been validated
  ([m220-positioning.md](m220-positioning.md)).
- **Bottom bleed (y = 32-34 mm): only with the `trailing` policy** (see
  The vertical policy), which prints it into the gap after the label,
  where it leaves no mark either.

By default (`clip`) the printer receives the label area plus the left
bleed: 42 x 30 mm, as long as an ordinary 40 x 30 mm print. In dots,
canvas columns 0-335 land on head dots 240-575, the label's left edge
(column 16) on dot 256 where an ordinary 40 x 30 mm print starts, and rows
16-255 are sent (up to 271 with `trailing`: 16-270 for a 271-row PAPPL
image).

## Designing

- Set the document or page size to **44 x 34 mm**, portrait, without
  margins, and place the label at x = 2-42 mm, y = 2-32 mm. The
  [template][] has the page, guides for the label and the bleed, and an
  empty Artwork layer. Its "Guides (hide before printing)" layer is
  locked and lies above Artwork; hide it before printing or exporting, as
  it is ordinary artwork otherwise.
- Keep text and anything that must not be cut off inside the label, with a
  margin: registration is only as good as the printer's gap detection.
- Let backgrounds run to the page's edges. The right 2 mm never prints;
  the top 2 mm does not print yet.
- Print at **100 %**: no "fit to page", "shrink to fit" or scaling in the
  application or the print dialog.

[template]: templates/m220-40x30-overprint-2mm.svg

The overprint layout is right only for a page rasterized at the printer's
203 dpi and 100 %. The driver checks what it can: a page whose raster
states a resolution other than 203 dpi, or that is not within 4 dots of
the 44 mm page's 352 dots wide, prints as an ordinary page, as it would
without overprint support, and the job log gets a warning: "The page is
the overprint design ..., but ...; printing it as an ordinary page."
Such a print is not anchored to the label. Artwork scaled down inside a
full-size page ("fit to page") cannot be detected: it prints with the
overprint layout, shrunk and shifted.

## Printing through CUPS

A CUPS queue offers the 44 x 34 mm size only if it was created after the
update; an existing queue keeps its old PPD. The server must already run
the new version; then recreate the queue with the service's port (and
environment; see the [README](../README.md)):

```bash
phomemo-printer-app register-cups --queue phomemo --port 8000 --replace
lpoptions -p phomemo -l | grep -o '44x34mm[.A-Za-z]*'
```

`--replace` recreates the queue, so its queue defaults are lost, and so
is its status as the default destination. An older queue can still print
the design as a custom 44 x 34 mm page size (`-o media=Custom.44x34mm`;
not yet tested).

Load the 40 x 30 mm labels on the printer's Media Setup page (see below),
then choose the `44x34mm.Borderless` page size in the print dialog (some
dialogs show it as 44 x 34 mm), or:

```bash
lp -d phomemo -o media=44x34mm.Borderless design.pdf
```

CUPS' driverless path does not pass the size's name on: it sends the
page's dimensions only (which arrive as `custom_44x34mm_44x34mm`), or no
size at all, leaving the page's raster header to tell. The driver
recognises the design by size: a page within 0.5 mm of 44 x 34 mm is the
overprint design **when the 40 x 30 mm label is loaded**, whatever its
name. With other media loaded the page prints as an ordinary label.

The vertical policy cannot be chosen per job through CUPS: its queues do
not forward the option. Set the printer's default instead (below).

## Printing directly

`phomemo-printer-app submit` (or any IPP client) can name the design size:

```bash
phomemo-printer-app submit -d m220 \
  -o media=om_40x30mm-overprint-2mm_44x34mm design.png
```

A job that names the design size while other media is loaded fails with a
message naming the labels to load, since no layout fits other stock.

PAPPL rasterizes PNG and JPEG images itself and places them by
`print-scaling`: an image prints 1:1 only if it is **351 x 271 pixels**
(the 44 x 34 mm page at 203 dpi, as PAPPL rounds it down) with a
resolution of at most 203 dpi, or none. Other images are scaled to fill
the page,
or, if they have a resolution and are smaller at it, centred at their own
size; either moves the artwork relative to the label. A 352 x 272 pixel
export, for instance, loses a column and a row to scaling. PAPPL has no
PDF filter: print PDFs through CUPS.

## The vertical policy

`phomemo-overprint-vertical` decides which rows of the design are sent:

| Keyword | Shown as | Rows sent |
| --- | --- | --- |
| `clip` (default) | Label only (top and bottom bleed not printed) | the label's, y = 2-32 mm |
| `trailing` | Label and bottom bleed (top bleed not printed) | y = 2-34 mm |

`trailing` prints the bottom bleed into the gap after the label, so the
bottom edge is covered too. It has been validated on two consecutive
labels: the bottom bleed reached the label's edge, the printer found the
next label as usual (none skipped, the second label's top clean), and on
both labels the frames printed as designed. Longer runs have not been
tested.

A job takes its own value if it has one, else the printer's default, else
`clip`; an unknown value is logged and taken as `clip`. A CUPS job cannot
carry the option, so the printer default is the only way to choose it for
CUPS jobs. Set it on the printer's **Printing Defaults** page (shown as
"Overprint (vertical)"), or from the command line on the machine running
the server:

```bash
phomemo-printer-app modify -d PRINTER \
  -o phomemo-overprint-vertical-default=trailing
```

Run it without `-u`: with `-u ipp://…` the server refused the request
("Unsupported printer-uri uri value"). A direct job can set its own:

```bash
phomemo-printer-app submit -d m220 \
  -o media=om_40x30mm-overprint-2mm_44x34mm \
  -o phomemo-overprint-vertical=trailing design.png
```

## Media Setup

The 44 x 34 mm design is a page size for designs, not media to load. The
printer's Media Setup page does not offer it, refuses it, and with the
40 x 30 mm labels loaded lists the designs they take, for instance:

> 40 x 30 mm + 2 mm overprint: design on 44 x 34 mm; the label is 2 mm
> from the left and 2 mm from the top; the right 2 mm is not printed.

It also shows the vertical policy jobs get by default, with a link to
Printing Defaults. A custom 40 x 30 mm size counts as the labels too.

If the design size was loaded anyway, through PAPPL's own Media page or
IPP, jobs take it as the 40 x 30 mm labels with a warning, and Media Setup
shows it as "40 x 30 mm + 2 mm overprint (load the 40 x 30 mm label
instead)"; saving the page unchanged loads the labels.

Overprint pages are tracked as the loaded labels are on Media Setup
(normally gap), whatever tracking the job asks for.

## Checking a job

The server's log (`journalctl -u phomemo-printer-app` for the service)
says how each page was printed. An overprint page logs, at the default
`info` level:

```text
Printing the overprint design om_40x30mm-overprint-2mm_44x34mm (40 x 30 mm
+ 2 mm overprint, matched by its media size) with vertical policy clip:
canvas columns 0-335 and rows 16-255, 30 bytes from the head's start.
```

"matched by" says which of the media name, media size or page size
identified the design. Without that line the page printed as an ordinary
label: look for a warning in its place, or, with
`PHOMEMO_LOG_LEVEL=debug`, a note such as "The job is the size of ...,
but ... is loaded", which is logged when other media is loaded.

## Limitations

- Only the M220 (`phomemo_m220`), and only 40 x 30 mm labels.
- Raw jobs (`application/vnd.phomemo-raw`) are sent as they are.
- A page turned a quarter turn (34 x 44 mm) is not the design.
- Registration along the feed depends on the printer's gap detection,
  whose repeatability was observed on two consecutive `trailing` labels
  only, by eye; the
  first print's position relative to the label varied by about 0.8 mm or
  more between continuous-mode passes
  ([m220-positioning.md](m220-positioning.md)). The top bleed is not
  printed, and with `clip` neither is the bottom bleed, so they do not
  protect against a label that sits off position along the feed.
