# Native display localization

The Astra ABI exposes three per-game fields:

- `text_encoding`: `cp932` (default) or `gbk`, applied only at native graph-text presentation boundaries through the existing VM graph-text decoding option.
- `font_path`: game-root-relative font file, empty by default to retain existing font selection. Declared with `CONFIG_FLAG_FILE_PATH | CONFIG_FLAG_GAME_RELATIVE`.
- `font_index`: zero-based SFNT/TTC face index, default `0`; a nonzero index requires an explicit font file. Declared as advanced with step `1`.

All fields carry Chinese labels, descriptions and groups (`语言`, `高级`); `text_encoding` choices are labelled `Shift-JIS（日文原版）` and `GBK（简体中文）`. The ABI v3 descriptor reports display name `ethornell-rs` and family name `Ethornell / BGI`.

The selected face is shared by GPU text rendering, CPU text composition, bitmap-text rasterization and glyph measurement. Missing files, invalid faces and missing required presentation glyphs fail explicitly instead of substituting a system font. Graph92 direct bitmap text uses the selected display encoding for byte-cell layout and glyph decoding.
Default CPU font discovery is lazy and owned by each runtime, replacing the process-global cache. The standalone native CLI's existing snapshot-font environment setting remains a default discovery input; an explicit ABI font overrides it. GPU default system-font discovery is unchanged.

Resource names, archive lookup/order, VM opcode decoding, internal string operations and choice/file targets keep their existing behavior. No archive decoder, protector runtime, Lua display hook or game-specific offset is added. Existing native archive and image readers continue to own standard resources, including BMP.

Public tests cover typed configuration/defaults, invalid font paths/faces, an original synthetic square-glyph SFNT fixture, selected-font CPU measurement/rasterization/coverage and GBK double-byte cells. Existing VM tests exercise GBK display decoding while preserving CP932 resource names and raw memory. These tests do not execute commercial games or protected binaries.

The HD external private-container consumer and additional archive ownership, and Sakura's protected private-resource consumer, remain separate conversion prerequisites. These display options do not establish complete localization support for either installation.

# Astra adapter package

`crates/astra-emu-ethornell-rs` is the AstraEMU C ABI v3 adapter: package `astra-emu-ethornell-rs`, library `astra_emu_ethornell_rs`, plugin id `ethornell-rs`. It exports `astra_emu_query`; the `diagnostic-bridge` feature is used for the dynamically loaded `astra_emu_ethornell_rs.dll`. Its only AstraEngine dependency is `astra-emu-abi` from `2018wzh/AstraEngine@6d9d691dcc8f3d768e40b5be1d6f37366cca62d8`.

Core-side changes for embedding are the shared standalone/embedded composition, the offscreen embedded frontend, typed host dialogs, per-runtime font discovery and session-owned audio shutdown in `ethornell-app`, `ethornell-render` and `ethornell-audio`.

# License text

`LICENSE-MPL-2.0` is the canonical Mozilla Public License 2.0 text, added to accompany the `MPL-2.0` declaration in the upstream root `[workspace.package]`. Upstream ships no license file.
