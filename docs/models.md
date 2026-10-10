# Supported printers

<!-- A unit test in phomemo-pappl/src/models.rs checks that this list, one
     line per head width, names exactly the models of the driver table. -->

- 96-dot head (12 mm; 13.5 mm on the 180 dpi D50): A30, D10, D20, D30, D31, D32, D35, D50, DM170, Q30
- 384-dot head (48 mm): M100, M102, M105, M108, M109, M110, M110S, M120, M126, M150
- 576-dot head (72 mm): M200, M200C, M206, M208, M209, M219, M220, M220C, M220S

Each listed model has a driver profile named after it, e.g. `phomemo_m220`.
These models are expected to work based on the vendor's commands and their
shared protocol. Only the M220 has been tested on hardware by the
maintainer, who owns that model. Reports from other models are welcome.

All profiles use Bluetooth Classic SPP over RFCOMM for Bluetooth printing;
the application does not implement Bluetooth Low Energy (BLE) printing.

The widths above describe the print head, not the loaded media or label
width. The 96-dot models take labels wider than their head and print each
page turned a quarter turn. Select the actual loaded label size when
setting up a printer.

Please share successes or problems through
[GitHub issues](https://github.com/mabl/phomemo-printer-app/issues/new).
Include your model, distribution, architecture, installation type (and
package format if applicable), application/package version, firmware if
known, loaded label dimensions and print results.
