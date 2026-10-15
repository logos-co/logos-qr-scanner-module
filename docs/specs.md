# qr_scanner_module

`qr_scanner_module` shows QR codes and reads QR codes through the camera for other modules and apps. It
does neither itself. It bundles the **Logos QR helper**, a small program that alone opens the camera, and
hands back only the text of each code it reads: the camera's pictures never leave the helper.

## Why a helper, and why one

- **The camera stays out of host apps.** Basecamp, `logos_host`, `ui-host` and `logoscore` carry no camera
  entitlement or usage string, and none is needed.
- **One identity.** On macOS the helper is an app, `LogosQR.app` (`co.logos.QRHelper`, "Logos QR"). It
  re-executes itself with its responsibility disclaimed (`responsibility_spawnattrs_setdisclaim`, looked up at
  run time), so macOS asks for the camera in the helper's name, whoever started it, and the grant is the
  helper's alone.
- **Built and signed once.** The helper is released on its own tags (`helper-vX.Y.Z`). This module pins one
  release by URL and sha256 in `helper.json` and copies it beside its plugin after the build's fixup, so
  strip and re-signing never touch it. A module update keeps the camera grant; only a helper update can ask
  again (an ad-hoc signed helper always does; a Developer ID signed one keeps it).
- **One camera at a time.** One session runs at a time; a second caller is told `busy`.

## Sessions

A caller starts a session, polls it for the text of the codes the camera read, reports its progress, and
stops it. Only the holder of a session's token can poll, report or stop it. The answer to an unknown session
and to someone else's is the same: `not authorized`.

A session nobody polls for 30 seconds has lost its caller: the next `start` ends it.

**Show, then scan.** A session that shows a request and reads its answer does one, then the other, so the
camera is on no longer than it has to be. The request is shown with the camera off. When the wallet shows its
answer, the person presses **Scan the answer** in the window (or Enter or Space there), or Enter in the
terminal presenter: the camera comes on and the request's codes go. **Show the request again** (or Enter
again) turns the camera off and shows them again. A session that only scans, such as reading a wallet's
account code, opens the camera at once.

| Method | Parameters | Answer |
|---|---|---|
| `scanner_info` | | `{ ok, version, helper }`: this module's version, and the path of the helper it runs once one has started |
| `start` | `{ title?, show?: { parts, interval_ms?, max_version? }, scan, hint? }` | `{ ok, session, token }`. `parts` are shown as QR codes in turn, `interval_ms` each (200 by default), each at most version `max_version` (25 by default), error correction L. `scan` opens the camera: at once when nothing is shown, else once the person presses. The caller's module name is added to the title; `hint` is the window's line while only scanning |
| `poll` | `session, token` | `{ ok, state, step, texts, error?, camera?, presenter, presenter_max_version? }`. `texts` holds each new code's text once. `state` is `starting`, `running`, `cancelled` (the person pressed Cancel), `failed` (with `error`) or `stopped`. `step` is `show` while a request is shown and the camera is off, then `scan` |
| `progress` | `session, token, { received, expected }` | `{ ok }`. Shown under the camera's picture |
| `stop` | `session, token` | `{ ok }`. Closes the helper |

Errors are `{ ok: false, code, error }`, with `code` one of `busy`, `invalid`, `not_authorized`, `helper`. A
session's own `error.code` comes from the helper: `camera_denied`, `no_camera`, `camera_failed`,
`no_window`, `helper`.

### Events

| Event | Arguments |
|---|---|
| `scanner_changed` | `session, state`: the session changed or read something. It never says what: callers poll with their token |

## The terminal presenter

Without a window, as on a Linux text console or under a daemon, a terminal can show the codes instead. The
module draws them; `tools/qr-present` and `tools/qr-present.ps1` only print what it returns, through plain
`logosctl call`.

| Method | Parameters | Answer |
|---|---|---|
| `presenter_attach` | | A handle, as plain text |
| `presenter_render` | `handle, cols, rows` | The frame due now, as plain text: each module two spaces painted black or white (256-colour background codes) on one row, so about square and drawn by no font; then a status line. While the camera reads, a status line only. Each call renews the presenter's claim |
| `presenter_press` | `handle` | `pressed`, or `nothing to press`: a key pressed in the terminal moves the session from its request to its answer, or back |
| `presenter_detach` | `handle` | `detached` |

While a presenter rendered in the last 10 seconds, new sessions start the helper with `--no-window`: the
camera still reads, and the codes go to the terminal. `poll` then answers `presenter: true` and the largest
code version the terminal holds, so a caller can make its parts smaller. That is never above version 5,
however large the terminal: a Keycard Shell read version 4 from a terminal well, version 6 with some trouble,
and the far denser code a large Linux console allowed only with great difficulty.

`tools/qr-present` and `tools/qr-present.ps1` take `QR_PRESENT_SIZE="rows cols"` to draw within a smaller
box, and so with smaller codes; `tools/qr-present` also takes `QR_PRESENT_HOLD` for how long each code stays
drawn, in tenths of a second (2 by default).

## The signing app

In Basecamp the signer app (`evm_signer_ui`) shows what is signed, so it also draws the codes that carry
it to a QR wallet and says how the answer is read: no helper window. It attaches as a presenter like a
terminal; the core naming the caller `evm_signer_ui` is what makes it this kind, and only this kind gets:

| Method | Parameters | Answer |
|---|---|---|
| `presenter_view` | `handle, frames_id` | `{ ok, active, state?, step?, in_window, title, hint?, label?, received?, expected?, seeing, frames_id, frames?, interval_ms }`, renewing the claim. `frames` (`[{ size, bits }]`, drawn in turn every `interval_ms`) come only when `frames_id` isn't the one given, so once a session. `label` is the button for the person's next step, which `presenter_press` takes. `in_window`: the session started before the app was drawing, and stays in the window |
| `presenter_cancel` | `handle` | `{ ok, cancelled }`: the app's Cancel ends the session it draws, as the window's does |

No picture leaves the helper for it: `seeing` says only whether a code is in the camera's view. While
the app's claim is fresh, a terminal can't take it (`presenter_attach` answers an error), and its codes
keep the size the caller gave them: an app's size doesn't shrink them as a terminal's does.

## The helper

`logos-qr-helper` speaks JSON lines: commands on its standard input, events on its standard output. It ends
when its standard input does, so it never outlives its module.

| Command | Fields |
|---|---|
| `show` | `title?, frames: [{ size, bits }], interval_ms`; no frames hides them |
| `scan` | `title?, hint?`: open the camera |
| `stop_scan` | Close the camera |
| `ask` | `label?, hint?`: a button that reports `pressed` when clicked, or on Enter or Space; no label removes it. `hint` replaces the first line |
| `progress` | `received, expected` |
| `title` | `title` |
| `close` | |

| Event | Fields |
|---|---|
| `ready` | `version, window, fake` |
| `camera` | `name` |
| `text` | `text`, once per distinct text |
| `seeing` | `code`: a code came into the camera's view, or left it |
| `pressed` | The button `ask` put up |
| `cancelled` | |
| `error` | `code, message` |

- **Camera.** SDL3's camera API, at the widest format up to 1920 pixels. Raw formats come as NV12, for its
  grayscale plane. MJPEG, which SDL can't convert but USB webcams often keep their HD modes for, is decoded
  by the helper itself (zune-jpeg, luma only); a raw format wins a tie. On macOS the helper asks for access
  itself first, since SDL relies on an implicit prompt. `LOGOS_QR_CAMERA` picks a camera by name; Tab
  switches cameras in the window.
- **Exposure, on Linux and Windows.** A webcam's auto exposure meters the whole room and turns a wallet's
  backlit screen white. While it scans, the helper sets the exposure itself. It searches on the middle of
  the picture, then meters inside the code it read and holds while parts arrive.
  - On Linux, through V4L2, in whole multiples of 5 ms (5 to 30 ms), since the Keycard Shell dims its
    backlight with 200 Hz PWM and other exposures band its codes.
  - On Windows, through DirectShow's `IAMCameraControl`, which holds while SDL streams the camera through
    Media Foundation. It offers powers of two only: 7.8, 15.6 and 31 ms, from 15.6 ms.
  - The camera's own settings are written to `logos-qr-exposure-<camera>` in `$XDG_RUNTIME_DIR` (else the
    temporary directory) before anything changes. They are given back when the camera closes, or by the
    next helper if one died.
  - `LOGOS_QR_EXPOSURE` pins one exposure for trying them by hand, in the camera's units: on Linux `50` is
    5 ms, on Windows `-6` is 2^-6 s.
- **Decoding.** zxing-cpp over the whole frame, then rqrr over its centre when zxing found nothing.
- **Stopping.** SIGTERM, SIGINT and SIGHUP stop the helper as `close` does, so the camera is given back.
- **Diagnosing a camera.** `LOGOS_QR_DUMP=frame.pgm` rewrites the picture the decoders see each second and
  lists the camera's modes on standard error; `logos-qr-helper --decode frame.pgm` runs the decoders over
  such a picture.
- **Window.** SDL3's renderer: the frames, the camera's picture framed green while a code is in view, the
  status, and Cancel (or Escape).
- **Fake mode.** `LOGOS_QR_HELPER_FAKE=host:port` replaces the camera and the window with a TCP peer: it is
  sent `{"show": {frames, interval_ms}}` and `{"scan": true}` / `{"scan": false}` lines, and sends `raster
  <w> <h>\n` followed by `w*h` grayscale bytes for the decoders to read (only while the camera is on), and
  `press\n` where a person would press the window's button.
- **`LOGOS_QR_HELPER`** makes this module run another helper, for development.

## Packaging

| System | Beside the plugin |
|---|---|
| macOS | `LogosQR.app`: Info.plist with `NSCameraUsageDescription`, the hardened runtime, `com.apple.security.device.camera` |
| Linux | `logos-qr-helper` |
| Windows | `logos-qr-helper.exe` |

Where `helper.json` pins no release for a system, the module carries a development build from source:
`co.logos.QRHelper.dev`, ad-hoc signed, for tests. A local build wrapped by `helper/scripts/make-app.sh`
gets the same `.dev` id, so it never touches the release's grant. On macOS the development build is sealed
like a release (rcodesign: the hardened runtime, the entitlement, Info.plist bound) and links only the
system's libraries. That way the portable package carries it unchanged: `nix-bundle-dir` copies a sealed
`.app` byte for byte and never rewrites or re-signs it. No Windows build is carried until a release
pins one.

## By hand

On a Mac with a camera, build the helper and run it with `--probe`: macOS asks in the helper's name, and the
window prints each code it reads on standard output.

```sh
cargo build --release --manifest-path helper/Cargo.toml
sh helper/scripts/make-app.sh helper/target/release/logos-qr-helper helper/target/app 0.1.0
helper/target/app/LogosQR.app/Contents/MacOS/logos-qr-helper --probe
```

Measured on 2026-10-14 on macOS 27.0.1 with a MacBook's FaceTime HD camera: the prompt named "Logos QR
(dev)", tccd attributed the access to the helper's bundle id, and the camera read a Keycard Shell's animated
account code, 70 parts in about a minute.
