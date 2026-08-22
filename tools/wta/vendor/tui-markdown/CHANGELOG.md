# Changelog

All notable changes to this project will be documented in this file.

## [unreleased]

## [0.3.9](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.8...tui-markdown-v0.3.9) - 2026-07-23

- Render GFM tables and alerts, raw HTML, math, footnotes, and definition lists ([#153], [#154]).
- Render configurable image text fallbacks using alt text or URLs ([#155], [#156]).
- Choose from bundled syntax-highlighting themes or load a custom TextMate theme ([#158], [#159]).
- Customize or hide heading markers and code-block fences, with style sheets that only override
  the choices they need ([#167], [#168]).
- Style visible link labels consistently with their URLs.
- Keep styled list content beside its marker ([#166]).

[#153]: https://github.com/joshka/tui-markdown/pull/153
[#154]: https://github.com/joshka/tui-markdown/pull/154
[#155]: https://github.com/joshka/tui-markdown/pull/155
[#156]: https://github.com/joshka/tui-markdown/pull/156
[#158]: https://github.com/joshka/tui-markdown/pull/158
[#159]: https://github.com/joshka/tui-markdown/pull/159
[#167]: https://github.com/joshka/tui-markdown/pull/167
[#168]: https://github.com/joshka/tui-markdown/pull/168
[#166]: https://github.com/joshka/tui-markdown/pull/166

## [0.3.8](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.7...tui-markdown-v0.3.8) - 2026-06-27

- Maintenance and dependency updates ([#116], [#120], [#132], [#133]).

[#116]: https://github.com/joshka/tui-markdown/pull/116
[#120]: https://github.com/joshka/tui-markdown/pull/120
[#132]: https://github.com/joshka/tui-markdown/pull/132
[#133]: https://github.com/joshka/tui-markdown/pull/133

## [0.3.7](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.6...tui-markdown-v0.3.7) - 2025-12-27

- Add custom style sheets and render metadata blocks, subscript, superscript, soft line breaks, and
  horizontal rules ([#80], [#107], [#108], [#109], [#110], [#111]).
- Preserve heading metadata and update compatibility to Ratatui 0.30 ([#105], [#106]).

[#80]: https://github.com/joshka/tui-markdown/pull/80
[#107]: https://github.com/joshka/tui-markdown/pull/107
[#108]: https://github.com/joshka/tui-markdown/pull/108
[#109]: https://github.com/joshka/tui-markdown/pull/109
[#110]: https://github.com/joshka/tui-markdown/pull/110
[#111]: https://github.com/joshka/tui-markdown/pull/111
[#105]: https://github.com/joshka/tui-markdown/pull/105
[#106]: https://github.com/joshka/tui-markdown/pull/106

## [0.3.6](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.5...tui-markdown-v0.3.6) - 2025-11-03

- Render task lists ([#99]).

[#99]: https://github.com/joshka/tui-markdown/pull/99

## [0.3.5](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.4...tui-markdown-v0.3.5) - 2025-05-07

- Maintenance and dependency updates ([#79]).

[#79]: https://github.com/joshka/tui-markdown/pull/79

## [0.3.4](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.3...tui-markdown-v0.3.4) - 2025-05-07

- Maintenance release ([#81], [#82]).

[#81]: https://github.com/joshka/tui-markdown/pull/81
[#82]: https://github.com/joshka/tui-markdown/pull/82

## [0.3.3](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.2...tui-markdown-v0.3.3) - 2025-03-11

- Maintenance and dependency updates ([#71]).

[#71]: https://github.com/joshka/tui-markdown/pull/71

## [0.3.2](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.1...tui-markdown-v0.3.2) - 2025-03-09

- Maintenance and dependency updates ([#64], [#66], [#70]).

[#64]: https://github.com/joshka/tui-markdown/pull/64
[#66]: https://github.com/joshka/tui-markdown/pull/66
[#70]: https://github.com/joshka/tui-markdown/pull/70

## [0.3.1](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.3.0...tui-markdown-v0.3.1) - 2024-12-17

- Render links as their visible label followed by the destination URL ([#62]).

[#62]: https://github.com/joshka/tui-markdown/pull/62

## [0.3.0](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.12...tui-markdown-v0.3.0) - 2024-11-20

- Update compatibility to Ratatui 0.29 ([#58]).

[#58]: https://github.com/joshka/tui-markdown/pull/58

## [0.2.12](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.11...tui-markdown-v0.2.12) - 2024-10-22

- Maintenance and dependency updates ([#54]).

[#54]: https://github.com/joshka/tui-markdown/pull/54

## [0.2.11](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.10...tui-markdown-v0.2.11) - 2024-10-13

- Syntax-highlight fenced code blocks ([#51]).

[#51]: https://github.com/joshka/tui-markdown/pull/51

## [0.2.10](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.9...tui-markdown-v0.2.10) - 2024-09-20

- Render headings, blockquotes, ordered and nested lists, strong, emphasized, and struck-through
  text, and Markdown line breaks ([#45]).
- Ignore unsupported Markdown constructs instead of panicking ([#45]).

[#45]: https://github.com/joshka/tui-markdown/pull/45

## [0.2.9](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.8...tui-markdown-v0.2.9) - 2024-09-20

- Render unordered lists ([#44]).

[#44]: https://github.com/joshka/tui-markdown/pull/44

## [0.2.8](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.7...tui-markdown-v0.2.8) - 2024-09-02

- Maintenance and dependency updates ([#38], [#41]).

[#38]: https://github.com/joshka/tui-markdown/pull/38
[#41]: https://github.com/joshka/tui-markdown/pull/41

## [0.2.7](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.6...tui-markdown-v0.2.7) - 2024-08-06

- Maintenance release.

## [0.2.6](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.5...tui-markdown-v0.2.6) - 2024-06-24

- Update Ratatui compatibility ([#29]).

[#29]: https://github.com/joshka/tui-markdown/pull/29

## [0.2.5](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.4...tui-markdown-v0.2.5) - 2024-06-08

- Correct links to the crate documentation.

## [0.2.4](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.3...tui-markdown-v0.2.4) - 2024-05-22

- Maintenance and dependency updates ([#24]).

[#24]: https://github.com/joshka/tui-markdown/pull/24

## [0.2.3](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.2...tui-markdown-v0.2.3) - 2024-04-24

- Update Markdown parser and Ratatui compatibility ([#19], [#20], [#23]).

[#19]: https://github.com/joshka/tui-markdown/pull/19
[#20]: https://github.com/joshka/tui-markdown/pull/20
[#23]: https://github.com/joshka/tui-markdown/pull/23

## [0.2.2](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.1...tui-markdown-v0.2.2) - 2024-02-29

- Clarify licensing and the project's experimental status.

## [0.2.1](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.2.0...tui-markdown-v0.2.1) - 2024-02-27

- Correct documentation links and package metadata.

## [0.2.0](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.1.1...tui-markdown-v0.2.0) - 2024-02-27

- Render fenced code blocks and preserve Markdown line breaks.

## [0.1.1](https://github.com/joshka/tui-markdown/compare/tui-markdown-v0.1.0...tui-markdown-v0.1.1) - 2024-02-27

- Introduce the Markdown-to-Ratatui renderer.

<!-- generated by git-cliff -->
