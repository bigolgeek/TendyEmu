# HD pack test fixture

A two-rule HD pack in the subset defined by `docs/hdpack-format.md`: one background tile (CHR ROM
tile `$12`, palette `0F 16 27 30`) drawn in `bg.png`, one sprite tile (tile `$2E`, palette
`FF 16 36 0F`) drawn in `spr.png`, both at scale 2. The `<overscan>` line is there on purpose: the
subset ignores it with a warning, so a test can assert the pack loads with exactly one.

The art was drawn for this test and is licensed MIT OR Apache-2.0, the same as the crate. It is not
game art; no third-party pack is vendored in this repository.

`bg/` is the `<background>` example of `docs/hdpack-format.md` §7: a 16x16 magenta square that is not
a NES palette colour, at scale 1 with brightness 0.5, so a test can see both the image and the shading.
