# Xross patch

Based on the published `gpui-component` 0.8.1 crate, under Apache-2.0.

Adds `DialogEntrance::Slide`, using the existing 250 ms downward entrance and
reversed exit, with constant surface, shadow, and backdrop opacity. The existing
entrance variants retain their behavior. Reduced motion and interrupted exits
continue to use the upstream dialog lifecycle.

Local changes are confined to `src/dialog/entrance.rs` and
`src/dialog/dialog.rs`, including headless animation regression tests.
Remove this patch when an upstream release provides equivalent slide-only motion.

The `dialog::dialog::tests` filter covers pure-slide position and opacity,
interrupted entry, reduced motion, alerts, and the existing dialog lifecycle.
Run it from a temporary standalone copy (with an empty `[workspace]` table),
using a dedicated `CARGO_TARGET_DIR` and this repository's applicable GPUI and
block path patches. The published crate omits sibling assets and fixtures needed
to compile its other tests; recreate the upstream `crates/assets`, `crates/base`,
`examples/fixtures`, and `themes` layout only in that temporary test directory.
