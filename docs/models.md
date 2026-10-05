# Supported printers

<!-- A unit test in phomemo-pappl/src/models.rs checks that this list, one
     line per head width, names exactly the models of the driver table. -->

- 96-dot head (12 mm; 13.5 mm on the 180 dpi D50): A30, D10, D20, D30, D31, D32, D35, D50, DM170, Q30
- 384-dot head (48 mm): M100, M102, M105, M108, M109, M110, M110S, M120, M126, M150
- 576-dot head (72 mm): M200, M200C, M206, M208, M209, M219, M220, M220C, M220S

Each model has a driver named after it, e.g. `phomemo_m220`. The M220 is
tested on hardware; the others are listed because the vendor's software
drives them with the same commands. The 96-dot models take labels wider
than their head and print each page turned a quarter turn.
