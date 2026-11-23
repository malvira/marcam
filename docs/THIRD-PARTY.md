# Third-party code

marcam itself is Apache-2.0. Most of what it depends on comes from crates.io and npm under their
own licences (see each crate's `license` in `Cargo.lock`'s sources, or `cargo metadata`). This page
lists the code that is notable or is shipped with marcam.

## STEP import: SolidCraft's kernel

STEP files are read with [SolidCraft](https://github.com/tangency-labs/solidcraft)'s B-rep kernel,
through the `marcam-step` crate (`crates/marcam-step`). The kernel is a cargo git dependency pinned
to one revision (see `crates/marcam-step/Cargo.toml`); nothing of it is copied into this
repository. It is built:

- into the `marcam-step` command (the marcam command line reading STEP parts too:
  `cargo install --path crates/marcam-step`; the published `marcam` does not include it) and the
  Python module's `step` feature;
- into the web app's STEP module (`web/pkg/marcam_step.wasm`, built by
  `scripts/build-step-wasm.sh`), which the browser fetches only when a STEP file is opened.

| Crate | From | Licence |
| --- | --- | --- |
| solidcraft-kernel, solidcraft-geom, solidcraft-profile, solidcraft-smlib | SolidCraft | MIT OR Apache-2.0 |
| solidcore, solidcore-record, solidcore-ops, solidcore-tess, solidcore-step, solidcore-convert | SolidCore (in SolidCraft's repository) | Apache-2.0 |
| spade (faces the kernel leaves out, triangulated by marcam-step itself; already in the kernel's tree) | Stefan Altmayer | MIT OR Apache-2.0 |

marcam uses only SolidCraft's open geometry code. SolidCraft's look and feel (its UI kit, icons,
theme) are under its separate brand licence (`LICENSE-BRAND` in its repository); marcam uses none
of it.

The licences of SolidCraft's own dependencies are listed in its `THIRD-PARTY-LICENSES.txt`.

### `web-time`

SolidCraft's kernel times itself with the `web-time` crate (MIT OR Apache-2.0). marcam replaces
it, through `[patch.crates-io]` in the workspace manifest, with its own stand-in
(`crates/web-time-shim`, Apache-2.0): `std::time` natively, a clock that stands still in
WebAssembly. This way the browser's STEP module needs no wasm-bindgen glue.

## Web app

three.js (MIT) is loaded from a CDN by the web app; it is not in this repository.
