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

The local `astra` branch derives from xmoezzz/ethornell-rs
`6550460beb2012781cdaffdf18da59780de422ef`. The local `master` branch preserves
that upstream revision. `astra-emu-ethornell` is a thin ABI adapter around the
original BGI VM, native media/audio/save implementation and GPU compositor.
The standalone and embedded frontends share graph, text, ruby and modeless-UI
composition. Embedded rendering reads the final offscreen GPU target; it does
not create a window or present.

The ABI exports `astra_emu_query`; static registration uses `QUERY`.
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

Build commands from this directory (the ABI dependency must exist at
`../../Source/FamilyApi/astra-emu-abi`):

```sh
cargo build -p astra-emu-ethornell --features diagnostic-bridge --target-dir target/dynamic
cargo build -p astra-emu-ethornell --no-default-features --target-dir target/static
cargo run -p ethornell-cli -- run --game /path/to/game
cargo run --manifest-path ../../Cargo.toml -p astra-emu-manager
git worktree add ../ethornell-original 6550460beb2012781cdaffdf18da59780de422ef
cargo run --manifest-path ../ethornell-original/Cargo.toml -p ethornell-cli -- run --game /path/to/game
```

For runtime acceptance, load `target/dynamic/debug/astra_emu_ethornell.dll`
in the integrated Astra Manager with the same game root as the standalone
command. Compare title/menu, dialogue,
choices, animated transitions, movie/audio, native save/load, pointer edges,
focus loss, pause/resume and close/reopen. Retain a CPU lease across close and
release it afterward. Cancel once during open and once during a modal wait.
Also compare against a separate checkout of the exact upstream revision.
These commands and the adapter are not evidence of an exercised game ending.

License provenance remains unresolved for publication: upstream's root
`[workspace.package]` declares `MPL-2.0`, but its crate manifests do not inherit
that license and the pinned tree contains no LICENSE/COPYING file or source
license grant. This adaptation preserves those declarations verbatim, adds
no relicensing claim, and must not be published until provenance is clarified.
