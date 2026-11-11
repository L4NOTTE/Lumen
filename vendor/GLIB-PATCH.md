# glib 0.18.5 security backport

Source: crates.io glib 0.18.5, with its original licenses preserved in glib/.
Only src/variant_iter.rs is changed: the g_variant_get_child output pointer
is mutable and passed as &mut p, matching the upstream correction.

References:
- https://rustsec.org/advisories/RUSTSEC-2024-0429.html
- https://github.com/gtk-rs/gtk-rs-core/pull/1343

The GTK3 stack cannot directly consume glib 0.20. This is a local backport,
not an upstream version upgrade; audit tools can still flag the version.
Native iterator regression tests must run on Linux before release.
