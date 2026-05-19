# aka.ms Shortlinks for Intelligent Terminal

This document lists every `aka.ms/intelligentterminal/*` shortlink the codebase
references. **All must be reserved via the [aka.ms portal](https://aka.ms/akams)
before the app ships** — until they're reserved they will 404.

## Required shortlinks

| Slug | Purpose | Where used in code | Target URL to point at |
|---|---|---|---|
| `aka.ms/intelligentterminal/feedback` | "Send Feedback" button in About dialog and Feedback menu item | `TerminalApp/AboutDialog.cpp`, `TerminalApp/Resources/uk-UA/Resources.resw` (FeedbackUriValue) | Your feedback intake (Feedback Hub deep link, GitHub issues, internal form, etc.) |
| `aka.ms/intelligentterminal/source` | "Source code" hyperlink in About dialog | `TerminalApp/AboutDialog.xaml` | https://dev.azure.com/microsoft/Dart/_git/IntelligentTerminal (or public mirror) |
| `aka.ms/intelligentterminal/docs` | "Documentation" hyperlink in About; `$help` URL embedded in serialized user settings | `TerminalApp/AboutDialog.xaml`, `TerminalSettingsModel/CascadiaSettingsSerialization.cpp` | Your docs root (when you publish one) |
| `aka.ms/intelligentterminal/releasenotes` | "Release notes" hyperlink in About dialog | `TerminalApp/AboutDialog.xaml` | Your release notes page |
| `aka.ms/intelligentterminal/privacy` | "Privacy Policy" hyperlink in About dialog and Store PDP | `TerminalApp/AboutDialog.xaml`, `build/StoreSubmission/Stable/PDPs/*/PDP.xml` | Your product-specific privacy statement |
| `aka.ms/intelligentterminal/schema` | `$schema` URL in serialized user settings (Release build) | `TerminalSettingsModel/CascadiaSettingsSerialization.cpp`, `UnitTests_SettingsModel/SerializationTests.cpp` | Raw URL to the JSON schema file for Intelligent Terminal profiles (e.g. raw GitHub/ADO URL) |
| `aka.ms/intelligentterminal/schema-preview` | `$schema` URL for Preview brand (if/when you ship Preview) | `TerminalSettingsModel/CascadiaSettingsSerialization.cpp` | Same as above but for Preview channel |
| `aka.ms/intelligentterminal/portable-mode` | Help link on the portable-mode disclaimer in Settings | All `TerminalSettingsEditor/Resources/*/Resources.resw` (16 locales) | Docs page explaining portable mode |
| `aka.ms/intelligentterminal/agent-actions` | "Learn more" disclaimer link on the Actions settings page | `TerminalSettingsEditor/Actions.xaml` | Docs page about AI agent actions |
| `aka.ms/intelligentterminal/extensions` | "Learn more about fragment extensions" link on Extensions settings page | `TerminalSettingsEditor/Extensions.xaml` | Docs page about fragment extensions |
| `aka.ms/intelligentterminal/legacy-globals` | Help link on legacy `globals` property upgrade hint (uk-UA only) | `TerminalApp/Resources/uk-UA/Resources.resw` (LegacyGlobalsPropertyHrefUrl) | Docs page about the JSON schema migration |

## Owner

TBD — assign one person to drive aka.ms reservations.

## Status

All 11 slugs are currently referenced in code as placeholders and will 404 until
the redirects are configured.
