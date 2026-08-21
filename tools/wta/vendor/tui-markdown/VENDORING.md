# Vendored tui-markdown

This directory is based on the crates.io release of `tui-markdown` 0.3.9:

- Archive: `tui-markdown-0.3.9.crate`
- Archive SHA-256: `A0C95AFC7B66008823DEA2614A800D944E18990690DB47517EFC66CD01FF9CEE`
- Upstream tag: https://github.com/joshka/tui-markdown/tree/tui-markdown-v0.3.9
- Upstream commit: `c2c5509ce03b531717fdc9b01eb0528dacdb67c8`

The commit immediately before the Intelligent Terminal customization contains
all 34 archive members byte-for-byte. Its deterministic manifest, formed from
sorted UTF-8 lines in the shape `<relative-path>\t<lowercase SHA-256>\n`, has
SHA-256 `FCA45F363927F0A8A30809D5448FB6FA03A2594B9368846E18842EDA48CF2139`.
Cargo's extraction-only `.cargo-ok` marker is not part of the archive or this
directory.

## Intelligent Terminal changes

Intelligent Terminal modifies the canonical event renderer to return top-level
source and rendered-line ranges from the same `pulldown-cmark` pass:

- `src/lib.rs` exports the source-map projection API.
- `src/renderer/mod.rs` collects source blocks and rendered-line ranges.
- Child renderers accept the shared mapped-event iterator instead of dropping
	source offsets at renderer boundaries.

The `.gitignore` and license files come from the same upstream tag. The
existing rendering APIs and behavior remain covered by the upstream test suite.
The upstream project is dual-licensed under MIT or Apache-2.0. See
`LICENSE-MIT` and `LICENSE-APACHE`.

## Updating

1. Download the target `.crate` release and verify its published checksum.
2. Commit the archive payload unchanged before applying local modifications.
3. Reapply the source-map commit and resolve upstream renderer API changes.
4. Run the vendored crate tests, WTA tests, and third-party notice generator.

To verify an import, extract the `.crate` with one stripped path component and
compare the sorted relative path set and SHA-256 of every file. Do not normalize
line endings or trailing whitespace: the 0.3.9 archive contains four such
whitespace findings, and the unchanged import intentionally preserves them.
