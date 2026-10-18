# ethornell-rs

`ethornell-rs` is a reimplementation of the Ethornell / BURIKO General Interpreter (a.k.a. "BGI") visual novel engine.

The runtime detects the native game identifier from the comparison in
`system.arc:ipl._bp`. If detection is unavailable, the identifier falls back to
`Tayutama2TV`. For example:

```sh
cargo run -p ethornell-cli -- run --game /path/to/sun_flower
```

`--game` locates the resource archives, loose files, and user data.

Desktop builds for Linux (x86_64 and AArch64), macOS (Apple Silicon and Intel),
and Windows (x86_64) are published on
[GitHub Releases](https://github.com/asukaminato0721/ethornell-rs/releases).
The `pre-release` entry is updated after successful branch pushes; `v*` tags
publish versioned releases.

The window uses the game's icon from `icon.png`, an `.ico` file, or the icon
resources embedded in its Windows executable, in that order.

For a GBK Chinese translation, also set `ETHORNELL_TEXT_ENCODING=gbk`.
This selects the encoding of text buffers passed to the graphics API, including
dialogue and speaker names. The default is Shift-JIS (`cp932`). It does not
change the encoding of resource names or BP system strings.

## Astra embedded adaptation

The published [`astra`](https://github.com/2018wzh/ethornell-rs/tree/astra)
branch derives from xmoezzz/ethornell-rs
`6550460beb2012781cdaffdf18da59780de422ef`. `astra-emu-ethornell` is a thin ABI adapter around the
original BGI VM, native media/audio/save implementation and GPU compositor.
The standalone and embedded frontends share graph, text, ruby and modeless-UI
composition. Embedded rendering reads the final offscreen GPU target; it does
not create a window or present.

The ABI exports `astra_emu_query`; static registration uses `QUERY`.
The descriptor and probe share the format identifier `bgi-bp-arc`. This is a
machine-readable ABI symbol, not a display label with spaces or slashes.
`diagnostic-bridge` is for dynamically loaded modules. Static builds reuse the
host tracing subscriber. Startup-only configuration fields are `trace` and
`fail_on_stub`; no text/resource hooks or live configuration are advertised.
Logical size L is the script's native screen size. Raster R equals L. Output O
is the host's actual client size and does not resize R. Pointer coordinates
already refer to the displayed raster; the adapter applies no second host
scaling. Primary/secondary/middle ABI buttons 1/2/3 map to native 1/2/4.
Keyboard semantics retain the original arrow/Enter/Space mapping and native
edit-text handling. Wheel values retain fractional line detents.

Each session owns a native worker, scheduler, GPU device and audio backend.
Pause/suspend freezes engine time and playing audio channels; focus loss
clears held input. Cancellation invalidates host window requests before join.
CPU frame leases own immutable pixels independently of session shutdown.
Close/reopen does not recreate an OS event loop. Native file/folder chooser
requests and desktop-wallpaper changes currently fail explicitly in embedded
mode because the host has no cancellable service for them; message, text and
bounded item dialogs use the typed host modal service.
Host failures retain the requested operation, flags and coordinates together
with the Host error code and message; failed window operations are not acknowledged.
Only native void cursor motion retains the fixed upstream
`6550460beb2012781cdaffdf18da59780de422ef` frontend policy
(`crates/ethornell-app/src/window.rs:389-397`): an execution failure is warned
and gameplay continues, without acknowledging a successful warp or updating
actual Host cursor state. Cancellation, stale context, malformed protocol and
other window/modal operations remain errors.

Dependencies are exact git revisions: `astra-emu-abi` from
`2018wzh/AstraEngine@15a39a626b1316cb6137a75e17e14a2ff2d1ba88` (C ABI 2), and Kira 0.8.7
from `2018wzh/kira@da8668fcd5e543eec2e9ea9871b310276e09fdc5`. The Kira fix
is separate from the adapter, based on upstream
`f46857e121872416fba0d539ff3d59aa99876fb8`, retaining its MIT/Apache-2.0 files.
It joins the desktop device watcher and adds an explicit gameplay-thread
decoder owner. Native audio shutdown drops the manager/device first, then
cancels and joins all registered movie-audio decoders. No joins run in the
realtime callback and dropping a playback handle still leaves playback alone.
The root patches CPAL 0.15.3 to
`2018wzh/cpal@65ac8a6ac1a635cc26bfc5af331295b052044908` (upstream
`ac6cbb2ba55e61665a35ab88ae136a83380d1354`, original Apache-2.0 license).
Its WASAPI stream destructor also joins when the Terminate channel is already
disconnected. Integrating workspaces must repeat this root-level patch because
Cargo ignores dependency-local patches.

Build commands from this directory:

```sh
cargo build --release -p astra-emu-ethornell --features diagnostic-bridge --target-dir target/dynamic
```

For manual acceptance, load `target/dynamic/release/astra_emu_ethornell.dll`
in the integrated Astra Manager using an authorized game copy and isolated user
files. Exercise title/menu, dialogue, native save/load, pause/resume and
close/reopen through the real Manager window. Run acceptance manually, without
automated input or screenshot drivers.

License provenance remains unresolved: upstream's root `[workspace.package]`
declares `MPL-2.0`, but its crate manifests do not inherit that license and the
pinned tree contains no LICENSE/COPYING file or source license grant.
The source adaptation is published to `astra` under explicit user authorization.
Publication does not resolve the original components' license coverage or grant
new rights; this adaptation preserves existing declarations without relicensing.
Binary redistribution remains pending provenance clarification.
