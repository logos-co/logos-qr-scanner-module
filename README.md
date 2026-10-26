# logos-qr-scanner-module

`qr_scanner_module` shows QR codes and reads QR codes through the camera, for any Logos module or app that
needs it: a QR wallet's signer, a wallet scanning an address. It bundles the **Logos QR helper**, which
alone opens the camera and hands back only the text of each code. See [docs/specs.md](docs/specs.md).

```sh
logosctl call qr_scanner_module start '{"title":"Sign","show":{"parts":["UR:BYTES/..."]},"scan":true}'
logosctl call qr_scanner_module poll <session> <token>     # { state, texts: [...] }
logosctl call qr_scanner_module stop <session> <token>
```

From a terminal with no window, `tools/qr-present` (or `tools\qr-present.ps1`) draws the codes through plain
`logosctl call`, while the helper reads the camera without a window.

A request and its answer are two steps: the request's codes are shown with the camera off, and the camera
comes on only when the person says the wallet shows its answer, with **Scan the answer** in the window or
Enter in the terminal.

## Layout

| Path | What |
|---|---|
| `rust-lib/` | The module: sessions, frames, the terminal presenter, the link to the helper |
| `helper/` | `logos-qr-helper`: SDL3 camera and window, zxing-cpp and rqrr, JSON lines on stdio |
| `helper.json` | The helper release this module ships, pinned by URL and sha256 |
| `tools/` | The terminal presenter scripts |

## Build and test

```sh
nix build .#lgx                       # the module, with its helper beside the plugin
nix build .#helper                    # the helper from source, for tests
cargo test --manifest-path helper/Cargo.toml
cargo test --manifest-path rust-lib/Cargo.toml --no-default-features
```

`cargo test` in `rust-lib` resolves the Rust SDK's manifest, so check out
[logos-rust-sdk](https://github.com/logos-co/logos-rust-sdk) as `logos-rust-sdk-src` beside it.

## Licence

MIT or Apache-2.0, at your option.
