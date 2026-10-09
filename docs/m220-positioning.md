# M220 paper positioning and leading-edge bleed

Printed differential measurements support **approximately 0.5 mm of forward
feed per `ESC d` unit in continuous mode** on the tested M220. Count 8 added
4.2 and 4.3 mm in two valid trials; count 4 added 2.0 mm. This is relative
movement between prints, not an absolute-position readout. The first marker's
position relative to the sticker varied between passes, so precise leading-edge
bleed remains unproven.

## Device and test setup

Experiments on 2026-10-08 used an M220 reporting firmware **3.0.1**, with
**40 x 30 mm gap labels**, over Bluetooth SPP on RFCOMM channel 1. Physical
movement and printed distances were observed/measured by the operator;
successful byte transfer or a silent command is not a movement measurement.

Supporting evidence came from earlier reverse-engineering of the Print Master
app (decompiled `M200Printer`, `M220Printer`, `InsSet`, `InsGet`,
`QuinPrinter`) and its paper-learn screen (`PaperLearnActivity`, string
`xb_removePaper`), which requires an **empty paper path** for the paper reset.

These findings are specific to the tested M220 firmware. A command name in
an app shared by multiple models does not establish support on every printer.

## Established baseline

For a 320-dot-wide bitmap, send **40 bytes per row**, with left margin
**32 bytes** (72-byte/576-dot head width - 40). The working sequence is:

```text
1F 11 24 20                 left margin, 32 bytes
[1F 11 0A]                  gap tracking, when selecting it explicitly
1B 40                       reset; wait at least 100 ms
1F 11 21 01                 one copy
1D 76 30 00 28 00 hL hH     actual-width raster, height in rows
<40 bytes per row>
```

Each tested print reported `1A 0F 0C` (print complete). Earlier work found
that center-padding a raster to the full 72-byte head width is not equivalent
to margin plus actual width and produces incorrectly positioned output.

## Motion and tracking-mode experiments

| Command | Test state | Physical observation | What is established |
| --- | --- | --- | --- |
| `BACK_PAPER`, `1F 11 2B` | After reset, before a 40 x 30 mm comparison print in gap mode | Reverse movement; comparison artwork started about **3.2 mm higher** | Backfeed can affect the following gap-mode print in this sequence. |
| `BACK_PAPER` | Idle, after `ESC d 1` in gap mode | First call moved about **3.2 mm backwards** | Consistent with the print comparison, but not a fixed-distance specification. |
| Consecutive `BACK_PAPER` calls | No reset or print between calls; each probe used a new connection | Second observation uncertain; third, against a new mark, appeared to move forward/backward and finish at the same position | Repeated calls do not establish additive 3.2 mm reverse steps. A sensor-reference routine is a hypothesis. |
| `AUTO_LOCATE`, `1F 11 25` | Idle at the backfed position | No visible movement | No additional positioning benefit demonstrated in that state. |
| `ESC J 8`, `1B 4A 08`; `ESC J 16`, `1B 4A 10` | Idle in gap mode | No visible movement | No fine-feed operation demonstrated. |
| Same `ESC J` commands | Idle in confirmed continuous mode | No visible movement | Gap mode alone does not explain the idle result. Support or another required state remains unknown. |
| `ESC d 1`, `1B 64 01` | Idle in gap mode | About **29 mm forward**, reaching a new label | Not demonstrated as fine feed in gap mode. |
| `ESC d 1` | Idle in confirmed continuous mode, two trials | Small movement, observed at the feed; net direction/distance could not be measured reliably | Qualitative only; use the printed differential test below for distance. |

Direct replies were not received for these motion commands. Silence does
not distinguish an ignored command from successful execution.

### Tracking-mode readback

The following mode changes and replies were observed repeatedly:

| Operation | Bytes | Reply to label-type query `1F 11 19` |
| --- | --- | --- |
| Select gap | `1F 11 0A` | `1A 0C 0A` |
| Select continuous | `1F 11 0B` | `1A 0C 0B` |
| Restore gap | `1F 11 0A` | `1A 0C 0A` |

This demonstrates usable tracking-mode readback on the tested firmware.
Continuous-mode probes restored and confirmed gap mode afterward.

## Printed feed measurements

### Method

The initial test prints three **320 x 16 dot** rasters in continuous mode,
**A**, **B** and **C**, whose markers occupy raster dots 24-103, 120-199 and
216-295 respectively. Each has a two-dot-thick horizontal reference line at
its first row and a letter below it. No reset, locating command, or tracking
change occurs between them.

1. Print A and wait for completion.
2. Print B without explicit feed and wait for completion.
3. Send `ESC d n`, wait two seconds, then print C and wait for completion.
4. Restore and confirm gap mode.

Measure distances **along the paper-feed axis**, using the same edge of
each horizontal line. If marks cross a label gap, measure on the intact
backing strip and include the gap; do not compare distances from separate
label edges.

```text
ordinary inter-print advance = distance(A, B)
additional explicit feed    = distance(B, C) - distance(A, B)
estimated feed per unit     = additional explicit feed / n
```

The differential assumes ordinary inter-print advance is consistent over
the two intervals. Repetition and a second n value check that assumption
and approximate proportionality; they do not reveal the firmware algorithm.

### Results

| Trial | n | A -> B | B -> C | Additional feed | Estimated feed/unit |
| --- | ---: | ---: | ---: | ---: | ---: |
| Initial measurement | 8 | 9.1 mm | 13.3 mm | **4.2 mm** | **0.53 mm** |
| Unaligned repeat | 8 | Not measurable | About 13.6 mm, difficult to measure | Not calculable | Not calculable |
| Pre-aligned repeat | 8 | Not measurable | Not measured separately | Not calculable | Not calculable |
| Repeat with 16 leading rows | 8 | 11.0 mm | 15.3 mm | **4.3 mm** | **0.54 mm** |
| Half-count with 16 leading rows | 4 | 10.7 mm | 12.7 mm | **2.0 mm** | **0.50 mm** |

Feed-per-unit estimates are rounded to 0.01 mm (from 0.1 mm readings).

The unaligned repeat completed all three prints, but A was almost entirely
before the sticker's leading edge; only the last pixels of its letter were
visible. It therefore does **not** verify the differential feed. This exposed
a starting-position limitation: returning to gap mode does not itself
establish that the next continuous print begins within an adhesive label.

The refined setup (`--align-label`) starts with a gap-mode reset and
`ESC d 1` to establish a fresh label reference, then switches to continuous
mode and sends `ESC d 4` before A as an approximately 2 mm header. These setup
movements are outside both measured intervals. There is still no reset or
tracking change between A, B, and C. Alignment is experimental; judge it from
the printed marks.

In the pre-aligned repeat the operator observed that a label was skipped, but
A was still clipped above the leading edge and the print looked the same as
the unaligned repeat. Thus this setup did **not** establish a useful printing
origin. Do not infer that pre-print continuous feed reliably changes the first
raster's origin.

The successful refinement uses **16 blank leading rows inside every raster**
instead of pre-print positioning. Each raster is 320 x 32 dots; its reference
line starts at row 16, nominally 2 mm after raster start. The marker body is
unchanged. Both intervals use equal-height rasters, so their difference
remains comparable. The ordinary A-to-B spacing grew from 9.1 to 11.0 mm
(+1.9 mm), consistent with the 16 added rows (nominally 2.0 mm). It requires
no extra gap feed.

This padded pass made all reference lines visible. A's line was less than
0.3 mm below the sticker's leading edge. The extra feed was **4.3 mm**, within
0.1 mm of the initial 4.2 mm measurement despite the changed raster height.
Padding therefore solved the measurement placement problem for this pass.

The final n=4 pass used the same 32-row padded rasters and added **2.0 mm**.
This supports approximately proportional movement at **0.5 mm per unit**.
The no-feed baseline differed by 0.3 mm between the padded passes (11.0
versus 10.7 mm), and the n=8 increments were slightly more than twice the n=4
increment. These are approximate manual measurements, not a precision
calibration.

A's line in the n=4 pass was about **1.1 mm** below the sticker's leading
edge, versus less than 0.3 mm in the n=8 pass. Thus the starting origin
changed by roughly **0.8 mm or more** despite identical padding and setup.
Relative-feed repeatability and absolute label registration are separate
issues.

All three valid estimates (0.53, 0.54, 0.50 mm/unit) are consistent with a
nominal four-dot unit, `4 * 25.4 / 203 ≈ 0.50 mm`, within manual measurement
error (±0.1 mm per distance gives roughly ±0.025-0.05 mm/unit). This is not an
established exact unit or a repeatability guarantee. All three valid passes
returned three print-complete reports and confirmed gap-mode restoration
afterward.

A 16-row raster is nominally 2.0 mm tall, yet A-to-B was 9.1 mm; with 32-row
rasters (4.0 mm) it was 11.0 and 10.7 mm. Each print therefore adds roughly
7 mm of firmware movement beyond its rows (6.7-7.1 mm in these passes).
Raster rows alone are not a paper-position counter.

## Sensor information and calibration recovery

### Sensor-information query

`SENSOR_INFO`, `1F 11 1D`, returned a previously undocumented 15-byte reply:

```text
Initial loaded:  1A 2D 02 03 CE 00 00 00 00 01 F8 00 00 00 00
Empty:           1A 2D 02 00 AB 00 00 00 00 03 CF 00 00 00 00
Reloaded:        1A 2D 02 03 C2 00 00 00 00 01 F8 00 00 00 00
```

The loaded readings differed slightly across an unload/reload and
substantially from the empty reading. Optical sensor readings are plausible,
but **the payload is not decoded and is not a demonstrated position counter**.

### Calibration incident and recovery

Sending `VERIFY_PAPER`, `1B 4E 10`, **with paper loaded was inappropriate**.
It returned `1A 16 02 8B 00 00`, followed by persistent alternating
`1A 06 88` (paper absent) and `1A 06 89` (paper present). The app's paper
dialog flickered. `ESC @` and a power cycle did not recover stable sensing.

The vendor's empty-printer reset procedure recovered it:

1. Close the phone app to release its Bluetooth connection.
2. Remove the roll and **all label/backing paper from the path**; close the
   lid.
3. Send `1B 4E 10` once with the printer empty.
4. Wait; the recovery reply was `1A 16 00 13 00 00`, and rapid alternating
   reports stopped.
5. Reload the roll and close the lid. The operator reported the error gone,
   and a read-only check returned stable paper-present reports.

This calibration is not a positioning primitive and should not be part
of a print sequence. The reproduction script below does not expose it.

## Implications for leading-edge bleed

The findings support investigating this sequence:

```text
gap mode / existing label reference
BACK_PAPER
continuous mode
ESC d n                    controlled forward compensation
gap mode
one raster containing top bleed plus original artwork
```

For illustration only, at about 0.5 mm/unit, `n=5` would nominally leave
about 0.7 mm (3.2 - 2.5) of earlier start. The 3.2 mm backfeed was measured
after a gap-mode reset, and the 0.5 mm/unit between continuous-mode prints;
idle continuous `ESC d 1` was not measurable. This combined sequence has
**not** been print-validated. In particular, it remains unknown whether
returning to gap mode and beginning a normal raster preserves that offset.

In continuous mode the raster origin was observed **before** the sticker's
leading edge (about 0.9-2 mm, varying between passes), so leading rows there
trim an early start rather than create one. Blank rows cannot move a gap-mode
start earlier. Neither approach has been tested in a bleed job, and the origin
varied by about 0.8 mm or more between passes. No unconditional four-edge
coverage or absolute Y-position control has been established.

## Reproducing the experiment

The standalone diagnostic is `scripts/m220_feed_probe.py`. It uses Python's
standard library and Linux RFCOMM sockets; the original Python project and
Pillow are not required.

See `--help` for dry-run and live commands. The default does not connect or
print. Without `--align-label`, a complete pass sends three short rasters and
a single `ESC d`; it checks firmware/lid/temperature/paper status, confirms
gap mode before and continuous mode after setup, waits for each print
completion, and restores and reads back gap mode. There are no automatic
retries. The default is 16 blank leading rows; `--leading-rows 0` reproduces
the original unpadded geometry, which can clip A above the sticker.

From the repository root:

```bash
# Dry-run: describe the markers without opening a connection.
python3 -B scripts/m220_feed_probe.py --feed 8 --leading-rows 16

# Repeat the feed-count-8 measurement with visible, padded markers.
python3 -B scripts/m220_feed_probe.py \
  --address XX:XX:XX:XX:XX:XX --feed 8 --leading-rows 16 --print

# A separate three-marker pass checks the half-sized feed count.
python3 -B scripts/m220_feed_probe.py \
  --address XX:XX:XX:XX:XX:XX --feed 4 --leading-rows 16 --print

# Offline tests; no Bluetooth connection or print.
python3 -B -m unittest discover -s scripts \
  -p 'test_m220_feed_probe.py' -v
```

The offline tests are not part of `make check`; run them directly as above.

Keep the phone app disconnected during a pass, load 40 x 30 mm labels, and
retain the backing strip for measurements. The observed A-to-C spans were
22.4 mm initially and 26.3 mm in the padded n=8 pass; the padded n=4 span was
23.4 mm. The two commands above together use about two label pitches, but
actual label consumption depends on starting position and firmware movement.
Do not assume each raster stays on the adhesive label.
`--align-label` additionally advances to a fresh reference and adds a header
before each pass, so it can use more paper than the unaligned sequence. It
failed to prevent clipping in the observed test and is retained only to
reproduce that failed setup; the commands above use within-raster padding
instead.

The print sequence follows the driver's encoder and transport.

## Open questions

- Precision and longer-run repeatability of continuous-mode `ESC d` feed.
- A repeatable first-raster origin relative to the label edge.
- Whether `BACK_PAPER` seeks a reference, and what state it requires.
- Whether forward compensation survives return to gap mode and raster start.
- Registration over consecutive bleed jobs.
- Physical right-edge clearance for horizontal bleed on these labels.
- Exact meaning of the `1A 2D` sensor payload.
- Whether `ESC J` is unsupported or requires an untested state.
