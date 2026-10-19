# Native display localization

The Astra ABI exposes three per-game fields:

- `text_encoding`: `cp932` (default) or `gbk`, applied only at native graph-text presentation boundaries through the existing VM graph-text decoding option.
- `font_path`: game-root-relative font file, empty by default to retain existing font selection.
- `font_index`: zero-based SFNT/TTC face index, default `0`; a nonzero index requires an explicit font file.

The selected face is shared by GPU text rendering, CPU text composition, bitmap-text rasterization and glyph measurement. Missing files, invalid faces and missing required presentation glyphs fail explicitly instead of substituting a system font. Graph92 direct bitmap text uses the selected display encoding for byte-cell layout and glyph decoding.
Default CPU font discovery is lazy and owned by each runtime, replacing the process-global cache. The standalone native CLI's existing snapshot-font environment setting remains a default discovery input; an explicit ABI font overrides it. GPU default system-font discovery is unchanged.

Resource names, archive lookup/order, VM opcode decoding, internal string operations and choice/file targets keep their existing behavior. No archive decoder, protector runtime, Lua display hook or game-specific offset is added. Existing native archive and image readers continue to own standard resources, including BMP.

Public tests cover typed configuration/defaults, invalid font paths/faces, an original synthetic square-glyph SFNT fixture, selected-font CPU measurement/rasterization/coverage and GBK double-byte cells. Existing VM tests exercise GBK display decoding while preserving CP932 resource names and raw memory. These tests do not execute commercial games or protected binaries.

The HD external private-container consumer and additional archive ownership, and Sakura's protected private-resource consumer, remain separate conversion prerequisites. These display options do not establish complete localization support for either installation.
