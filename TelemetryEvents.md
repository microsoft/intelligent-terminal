# Windows Terminal telemetry events

This document enumerates every **true telemetry event** in the Windows Terminal codebase under `src\` — i.e., every `TraceLoggingWrite(...)` call site whose argument list contains one of the Microsoft telemetry keywords (`MICROSOFT_KEYWORD_MEASURES`, `MICROSOFT_KEYWORD_TELEMETRY`, or `MICROSOFT_KEYWORD_CRITICAL_DATA`) and is therefore reported to Microsoft.

Events grouped by their `TRACELOGGING_DEFINE_PROVIDER`. Diagnostic-only ETW traces tagged with `TIL_KEYWORD_TRACE` (`UiaTracing.cpp`, `parser/tracing.cpp`, most of `host/tracing.cpp`, the server `*Dispatchers.cpp`, `VtIo.cpp`, `VtInputThread.cpp`, etc.) are excluded.

Conventions used in the field tables below:
- **Type** uses the trailing portion of the `TraceLogging<Type>(...)` macro (e.g., `TraceLoggingBool` → `Bool`, `TraceLoggingValue` → `Value (auto)`).
- **Description** is the optional 3rd-argument string literal on the metadata macro; a dash (`—`) means none was supplied.
- **Source expression** is paraphrased; literal constants are shown in quotes.
- **Source links** are pinned to commit [`fb71a04`](https://github.com/microsoft/terminal/tree/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e) so that the line numbers stay accurate even as the codebase evolves.

---

## Provider: Microsoft.Windows.Terminal.App

- **Symbol:** `g_hTerminalAppProvider`
- **GUID:** `{24a1622f-7da7-5c77-3303-d850bd1ab2ed}`
- **Defined in:** [`src\cascadia\TerminalApp\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ActionDispatched`
- **Description:** Event emitted when an action was successfully performed.
- **Source:** [`src\cascadia\TerminalApp\ShortcutActionDispatch.cpp:67`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/ShortcutActionDispatch.cpp#L67)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Action` | Value (int) | `static_cast<int>(actionAndArgs.Action())` | — |
  | `Branding` | Value | `branding` (build branding string) | — |

### `AppCreated`
- **Description:** Event emitted when the application is started.
- **Source:** [`src\cascadia\TerminalApp\AppLogic.cpp:193`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/AppLogic.cpp#L193)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabsInTitlebar` | Bool | `_settings.GlobalSettings().ShowTabsInTitlebar()` | — |

### `AppInitialized`
- **Description:** Event emitted once the app is initialized.
- **Source:** [`src\cascadia\TerminalApp\AppLogic.cpp:475`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/AppLogic.cpp#L475)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `latency` | Float32 | `latency` (initialization latency) | — |

### `CommandPaletteDismissed`
- **Description:** Event emitted when the user dismisses the Command Palette without selecting an action.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:913`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L913)
- **Fields:** _(none)_

### `CommandPaletteDispatchedAction`
- **Description:** Event emitted when the user selects an action in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:801`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L801)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SearchTextLength` | UInt32 | `searchTextLength` | Number of characters in the search string. |
  | `NestedCommandDepth` | UInt32 | `nestedCommandDepth` | The depth in the tree of commands for the dispatched action. |

### `CommandPaletteDispatchedCommandline`
- **Description:** Event emitted when the user runs a commandline in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:873`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L873)
- **Fields:** _(none)_

### `CommandPaletteOpened`
- **Description:** Event emitted when the Command Palette is opened.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:64`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L64)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Mode` | WideString | `L"Action"` (literal) | Which mode the palette was opened in. |

### `ConnectionCreated`
- **Description:** Event emitted upon the creation of a connection.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1617`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1617)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ConnectionTypeGuid` | Guid | `connectionType` | The type of the connection. |
  | `ProfileGuid` | Guid | `profile.Guid()` | The profile's GUID. |
  | `SessionGuid` | Guid | `connection.SessionId()` | The `WT_SESSION`'s GUID. |

### `NewTabByDragDrop`
- **Description:** Event emitted when the user drag&drops onto the new tab button.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:591`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L591)
- **Fields:** _(none)_

### `NewTabMenuClosed`
- **Description:** Event emitted when the new tab menu is closed.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1110`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1110)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuCreatedNewTerminalSession`
- **Description:** Event emitted when a new terminal was created via the new tab menu.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1498`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1498)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NewTabCount` | Value (int) | `NumberOfTabs()` | The count of tabs currently opened in this window. |
  | `SessionType` | Value | `sessionType` | The type of session that was created. |

### `NewTabMenuDefaultButtonClicked`
- **Description:** Event emitted when the default button from the new tab split button is invoked.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:400`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L400)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuItemClicked`
- **Description:** Event emitted when an item from the new tab menu is invoked.
- **Source (5 call sites, distinct `ItemType` constant per call site):**
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1322`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1322) — `ItemType = "Profile"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1377`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1377) — `ItemType = "Action"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1721`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1721) — `ItemType = "Settings"` (also adds the extra `SettingsTarget` field)
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1743`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1743) — `ItemType = "CommandPalette"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1764`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1764) — `ItemType = "About"`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` / `NumberOfTabs()` | The count of tabs currently opened in this window. |
  | `ItemType` | Value (string) | `"Profile"` / `"Action"` / `"Settings"` / `"CommandPalette"` / `"About"` | The type of item that was clicked in the new tab menu. |
  | `SettingsTarget` | Value (string) | `targetAsString` (only for the `"Settings"` variant at line 1721) | The target settings file or UI. |

### `NewTabMenuItemElevateSubmenuItemClicked`
- **Description:** Event emitted when the elevate submenu item from the new tab menu is invoked.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:5935`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L5935)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuOpened`
- **Description:** Event emitted when the new tab menu is opened.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1092`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1092)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `QuickFixSuggestionUsed`
- **Description:** Event emitted when a winget suggestion is used.
- **Source (2 call sites, distinct `Source` constant per call site):**
  - [`src\cascadia\TerminalApp\SuggestionsControl.cpp:739`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L739) — `Source = "SuggestionsUI"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:5667`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L5667) — `Source = "QuickFixMenu"`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Source` | Value (string) | `"SuggestionsUI"` / `"QuickFixMenu"` | — |

### `SuggestionsControlDismissed`
- **Description:** Event emitted when the user dismisses the Command Palette without selecting an action.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:798`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L798)
- **Fields:** _(none)_

### `SuggestionsControlDispatchedAction`
- **Description:** Event emitted when the user selects an action in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:749`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L749)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SearchTextLength` | UInt32 | `searchTextLength` | Number of characters in the search string. |
  | `NestedCommandDepth` | UInt32 | `nestedCommandDepth` | The depth in the tree of commands for the dispatched action. |

### `SuggestionsControlOpened`
- **Description:** Event emitted when the Command Palette is opened.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:86`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L86)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Mode` | WideString | `L"Action"` (literal) | Which mode the palette was opened in. |

### `TabRenamerClosed`
- **Description:** Event emitted when the tab renamer is closed.
- **Source:** [`src\cascadia\TerminalApp\TabHeaderControl.cpp:112`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TabHeaderControl.cpp#L112)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `CancelledRename` | Boolean | `_renameCancelled` | True if the user cancelled the rename, false if they committed. |

### `TabRenamerOpened`
- **Description:** Event emitted when the tab renamer is opened.
- **Source:** [`src\cascadia\TerminalApp\TabHeaderControl.cpp:85`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TabHeaderControl.cpp#L85)
- **Fields:** _(none)_

### `WindowCreated`
- **Description:** Event emitted when the window is started.
- **Source:** [`src\cascadia\TerminalApp\TerminalWindow.cpp:226`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalWindow.cpp#L226)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Settings.Editor

- **Symbol:** `g_hTerminalSettingsEditorProvider`
- **GUID:** `{1b16317d-b594-51f8-c552-5d50572b5efc}`
- **Defined in:** [`src\cascadia\TerminalSettingsEditor\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `AddNewProfile`
- **Description:** Event emitted when the user adds a new profile.
- **Source (2 call sites, distinct `Type` constant per call site):**
  - [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:45`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L45) — `Type = "EmptyProfile"`
  - [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:62`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L62) — `Type = "Duplicate"` (also adds `SourceProfileHasSource`)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Type` | Value (string) | `"EmptyProfile"` / `"Duplicate"` | The type of the creation method (i.e. empty profile, duplicate). |
  | `SourceProfileHasSource` | Value (bool) | `!selectedProfile.Source().empty()` (only for the `"Duplicate"` variant at line 62) | True if the source profile has a `source` (i.e. dynamic profile generator namespace, fragment). Otherwise, false, indicating it's based on a custom profile. |

### `CreateUnfocusedAppearance`
- **Description:** Event emitted when the user creates an unfocused appearance for a profile.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Profiles_Appearance.cpp:87`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Appearance.cpp#L87)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `IsProfileDefaults` | Value (bool) | `_Profile.IsBaseLayer()` | If the modified profile is the `profile.defaults` object. |
  | `ProfileGuid` | Value (GUID) | `static_cast<GUID>(_Profile.Guid())` | The guid of the profile that was navigated to. |
  | `ProfileSource` | Value (wide string) | `_Profile.Source().c_str()` | The source of the profile that was navigated to. |

### `DeleteProfile`
- **Description:** Event emitted when the user deletes a profile.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Profiles_Base.cpp:93`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base.cpp#L93)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ProfileGuid` | Value (string) | `to_hstring(_Profile.Guid()).c_str()` | The guid of the profile that was navigated to. |
  | `ProfileSource` | Value (wide string) | `_Profile.Source().c_str()` | The source of the profile that was navigated to. |
  | `Orphaned` | Value (bool) | `false` (literal) | Tracks if the profile is orphaned. |
  | `Hidden` | Value (bool) | `_Profile.Hidden()` | Tracks if the profile is hidden. |

### `NavigatedToPage`
- **Description:** Event emitted when the user navigates to a page in the settings UI.
- **Source (17 call sites; each call site emits a distinct `PageId` constant — see the list below):**

  | Source | `PageId` constant | Extra fields beyond `PageId` |
  |---|---|---|
  | [`src\cascadia\TerminalSettingsEditor\Launch.cpp:47`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Launch.cpp#L47) | `"startup"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Interaction.cpp:28`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Interaction.cpp#L28) | `"interaction"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Extensions.cpp:53`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Extensions.cpp#L53) | `"extensions.extensionView"` | `FragmentSource`, `FragmentCount`, `Enabled` |
  | [`src\cascadia\TerminalSettingsEditor\Extensions.cpp:66`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Extensions.cpp#L66) | `"extensions"` | `ExtensionPackageCount`, `ProfilesModifiedCount`, `ProfilesAddedCount`, `ColorSchemesAddedCount` |
  | [`src\cascadia\TerminalSettingsEditor\GlobalAppearance.cpp:30`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/GlobalAppearance.cpp#L30) | `"globalAppearance"` | — |
  | [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:33`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L33) | `"addProfile"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Actions.cpp:37`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Actions.cpp#L37) | `"actions"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:62`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L62) | `"compatibility"` | — |
  | [`src\cascadia\TerminalSettingsEditor\ColorSchemes.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/ColorSchemes.cpp#L48) | `"colorSchemes"` | — |
  | [`src\cascadia\TerminalSettingsEditor\EditColorScheme.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/EditColorScheme.cpp#L48) | `"colorSchemes.editColorScheme"` | `SchemeName` |
  | [`src\cascadia\TerminalSettingsEditor\NewTabMenu.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/NewTabMenu.cpp#L48) | `"newTabMenu"` or `"newTabMenu.folderView"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Advanced.cpp:33`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Advanced.cpp#L33) | `"profile.advanced"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Appearance.cpp:65`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Appearance.cpp#L65) | `"profile.appearance"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource`, `HasBackgroundImage`, `HasUnfocusedAppearance` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Base.cpp:59`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base.cpp#L59) | `"profile"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Base_Orphaned.cpp:44`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base_Orphaned.cpp#L44) | `"profileOrphaned"` | `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Terminal.cpp:27`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Terminal.cpp#L27) | `"profile.terminal"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Rendering.cpp:23`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Rendering.cpp#L23) | `"rendering"` | — |

- **Fields (union across all variants):**

  | Name | Type | Description |
  |---|---|---|
  | `PageId` | Value (string) | The identifier of the page that was navigated to. |
  | `IsProfileDefaults` | Value (bool) | If the modified profile is the `profile.defaults` object. |
  | `ProfileGuid` | Value (GUID) | The guid of the profile that was navigated to. (For `Profiles_Base`, set to `{3ad42e7b-e073-5f3e-ac57-1c259ffa86a8}` if the `profiles.defaults` object is being modified.) |
  | `ProfileSource` | Value (wide string) | The source of the profile that was navigated to. |
  | `HasBackgroundImage` | Value (bool) | If the profile has a background image defined. |
  | `HasUnfocusedAppearance` | Value (bool) | If the profile has an unfocused appearance defined. |
  | `SchemeName` | Value (string) | The name of the color scheme that's being edited. |
  | `FragmentSource` | Value (wide string) | The source of the fragment included in this extension package. |
  | `FragmentCount` | Value (int) | The number of fragments included in this extension package. |
  | `Enabled` | Value (bool) | The enabled status of the extension. |
  | `ExtensionPackageCount` | Value (int) | The number of extension packages displayed. |
  | `ProfilesModifiedCount` | Value (int) | The number of profiles modified by enabled extensions. |
  | `ProfilesAddedCount` | Value (int) | The number of profiles added by enabled extensions. |
  | `ColorSchemesAddedCount` | Value (int) | The number of color schemes added by enabled extensions. |

### `OpenJson`
- **Description:** Event emitted when the user clicks the Open JSON button in the settings UI.
- **Source:** [`src\cascadia\TerminalSettingsEditor\MainPage.cpp:417`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/MainPage.cpp#L417)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SettingsTarget` | Value (string) | `target == SettingsTarget::DefaultsFile ? "DefaultsFile" : "SettingsFile"` | The target settings file. |

### `ResetApplicationState`
- **Description:** Event emitted when the user resets their application state.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:29`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L29)
- **Fields:** _(none)_

### `ResetToDefaultSettings`
- **Description:** Event emitted when the user resets their settings to their default value.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:41`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L41)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Setting.Model

- **Symbol:** `g_hSettingsModelProvider`
- **GUID:** `{be579944-4d33-5202-e5d6-a7a57f1935cb}`
- **Defined in:** [`src\cascadia\TerminalSettingsModel\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `DefaultTerminalChanged`
- **Description:** _(no `TraceLoggingDescription` supplied.)_
- **Source:** [`src\cascadia\TerminalSettingsModel\DefaultTerminal.cpp:102`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/DefaultTerminal.cpp#L102)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TerminalName` | WideString | `term.Name().c_str()` | The name of the default terminal. |
  | `TerminalVersion` | WideString | `term.Version().c_str()` | The version of the default terminal. |
  | `TerminalAuthor` | WideString | `term.Author().c_str()` | The author of the default terminal. |

### `JsonSettingsChanged`
- **Description:** Event emitted when `settings.json` change[s].
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1930`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1930)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Setting` | Value (string) | `change.data()` | — |
  | `Branding` | Value | `branding` (build branding) | — |
  | `Distribution` | Value | `distribution` (build distribution) | — |

### `MarksProfilesUsage`
- **Description:** Event emitted upon settings load, containing the number of profiles opted-in to scrollbar marks.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1378`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1378)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NumberOfAutoMarkPromptsProfiles` | Int32 | `totalAutoMark` | Number of profiles for which `AutoMarkPrompts` is enabled. |
  | `NumberOfShowMarksProfiles` | Int32 | `totalShowMarks` | Number of profiles for which `ShowMarks` is enabled. |

### `SendInputUsage`
- **Description:** Event emitted upon settings load, containing the number of `sendInput` actions a user has.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1361`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1361)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NumberOfSendInputActions` | Int32 | `collectSendInput()` | Number of `sendInput` actions in the user's settings. |

### `ThemesInUse`
- **Description:** Data about the themes in use.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1337`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1337)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ThemeClass` | Int32 | `themeChoice` | Identifier for the theme chosen. `0` = system (legacySystem = 6), `1` = light (legacyLight = 5), `2` = dark (legacyDark = 4), `3` = any custom theme. |
  | `ChangedTheme` | Bool | `changedTheme` | True if the user actually changed the theme from the default theme. |
  | `NumberOfThemes` | Int32 | `numThemes` | Number of themes in the user's settings. |

### `UISettingsChanged`
- **Description:** Event emitted when settings change via the UI.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1941`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1941)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Setting` | Value (string) | `change.data()` | — |
  | `Branding` | Value | `branding` | — |
  | `Distribution` | Value | `distribution` | — |

---

## Provider: Microsoft.Windows.Terminal.Connection

- **Symbol:** `g_hTerminalConnectionProvider`
- **GUID:** `{e912fe7b-eeb6-52a5-c628-abe388e5f792}`
- **Defined in:** [`src\cascadia\TerminalConnection\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/init.cpp)
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ConPtyConnected`
- **Description:** Event emitted when ConPTY connection is started.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:185`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L185)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Client` | WideString | `_clientName.c_str()` | The attached client process. |

### `ConPtyConnectedToDefterm`
- **Description:** Event emitted when ConPTY connection is started, for a defterm session.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:433`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L433)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Client` | WideString | `_clientName.c_str()` | The attached client process. |

### `ReceivedFirstByte`
- **Description:** An event emitted when the connection receives the first byte.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:785`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L785)
- **Privacy tag:** `PDT_ProductAndServicePerformance` _(differs from the rest of this provider)_
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Duration` | Float64 | `delta.count()` | — |

### `ReceiveTerminalHandoff_Success`
- **Description:** Successfully received a terminal handoff.
- **Source:** [`src\cascadia\TerminalConnection\CTerminalHandoff.cpp:93`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/CTerminalHandoff.cpp#L93)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:** _(none)_

---

## Provider: Microsoft.Terminal.Core

- **Symbol:** `g_hCTerminalCoreProvider`
- **GUID:** `{103ac8cf-97d2-51aa-b3ba-5ffd5528fa5f}`
- **Defined in:** [`src\cascadia\TerminalCore\TerminalApi.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalCore/TerminalApi.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

> **Registered by:** [`src\cascadia\TerminalControl\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp) (alongside `g_hTerminalControlProvider`).

### `ShellIntegrationWorkingDirSet`
- **Description:** The CWD was set by the client application.
- **Source:** [`src\cascadia\TerminalCore\TerminalApi.cpp:224`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalCore/TerminalApi.cpp#L224)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Win32Host

- **Symbol:** `g_hWindowsTerminalProvider`
- **GUID:** `{56c06166-2e2e-5f4d-7ff3-74f4b78c87d6}`
- **Defined in:** [`src\cascadia\WindowsTerminal\main.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/main.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ExeCreated`
- **Description:** Event emitted when the terminal process is started.
- **Source:** [`src\cascadia\WindowsTerminal\main.cpp:90`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/main.cpp#L90)
- **Fields:** _(none)_

### `SessionBecameInteractive`
- **Description:** Event emitted when the session was interacted with.
- **Source:** [`src\cascadia\WindowsTerminal\WindowEmperor.cpp:602`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/WindowEmperor.cpp#L602)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Branding` | Value | `branding` | — |
  | `Distribution` | Value | `distribution` | — |

---

## Provider: Microsoft.Windows.Console.Host

- **Symbol:** `g_hConhostV2EventTraceProvider`
- **GUID:** `{fe1ff234-1f09-50a8-d38d-c44fab43e818}`
- **Defined in:** [`src\host\tracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/tracing.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

> Most events written to this provider are diagnostic ETW traces (tagged with `TIL_KEYWORD_TRACE`) and are out of scope. Only the three telemetry-keyword events below are listed.

### `ConsoleHandoffFailed`
- **Description:** Failed while attempting handoff.
- **Source:** [`src\server\IoDispatchers.cpp:385`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L385)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |
  | _(unnamed)_ | HResult | `hr` | — |

### `ConsoleHandoffSessionStarted`
- **Description:** A new interactive console session was started.
- **Source:** [`src\server\IoDispatchers.cpp:275`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L275)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |
  | `handoffTargetChosenByWindows` | Bool | `handoffTargetChosenByWindows` | — |

### `ConsoleHandoffSucceeded`
- **Description:** Successfully handed off console connection.
- **Source:** [`src\server\IoDispatchers.cpp:365`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L365)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |

---

## Provider: Microsoft.Windows.Console.Launcher

- **Symbol:** `g_ConhostLauncherProvider`
- **GUID:** `{770aa552-671a-5e97-579b-151709ec0dbd}`
- **Defined in:** [`src\host\exe\exemain.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/exe/exemain.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`

### `IsLegacyLoaded`
- **Description:** _(no `TraceLoggingDescription` supplied.)_ Indicates that the legacy `ConhostV1.dll` console host was loaded by the launcher.
- **Source:** [`src\host\exe\exemain.cpp:150`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/exe/exemain.cpp#L150)
- **Keyword:** `MICROSOFT_KEYWORD_TELEMETRY` _(differs from the other providers, which all use `MICROSOFT_KEYWORD_MEASURES`)_
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ConsoleLegacy` | Bool | `true` (literal) | — |

---

## Cross-provider: WIL fallback failure event

The helper `Microsoft::Console::ErrorReporting::EnableFallbackFailureReporting(<provider>)` (see [`src\inc\WilErrorReporting.h`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/inc/WilErrorReporting.h)) installs a WIL fallback that emits a single named telemetry event whenever a WIL `THROW_…` / `LOG_…` macro reports a failure that hasn't already been logged. The event is written to **whichever provider was most recently passed to `EnableFallbackFailureReporting`** in the current module.

Each Terminal DLL/EXE in this list passes its own provider, so this event can appear under any of them:
- `Microsoft.Windows.Terminal.App` ([`src\cascadia\TerminalApp\init.cpp:22`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/init.cpp#L22))
- `Microsoft.Windows.Terminal.Settings.Editor` ([`src\cascadia\TerminalSettingsEditor\init.cpp:23`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/init.cpp#L23))
- `Microsoft.Windows.Terminal.Setting.Model` ([`src\cascadia\TerminalSettingsModel\init.cpp:22`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/init.cpp#L22))
- `Microsoft.Windows.Terminal.Connection` ([`src\cascadia\TerminalConnection\init.cpp:24`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/init.cpp#L24))
- `Microsoft.Windows.Terminal.Control` ([`src\cascadia\TerminalControl\init.cpp:26`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp#L26))

### `FallbackError`
- **Description:** WIL-reported failure that was not already reported elsewhere. (HRESULT `0x80131515` — XAML accessibility — is filtered out.)
- **Source:** [`src\inc\WilErrorReporting.h:32`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/inc/WilErrorReporting.h#L32)
- **Privacy tag:** `PDT_ProductAndServicePerformance`
- **Keyword:** `MICROSOFT_KEYWORD_TELEMETRY`
- **Level:** `WINEVENT_LEVEL_ERROR`
- **Fields** (all wrapped inside a `TraceLoggingStruct(14, "wilResult")`):

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `hresult` | UInt32 | `failure.hr` | Failure error code. |
  | `fileName` | String | `failure.pszFile` | Source code file name where the error occurred. |
  | `lineNumber` | UInt32 | `failure.uLineNumber` | Line number within the source code file where the error occurred. |
  | `module` | String | `failure.pszModule` | Name of the binary where the error occurred. |
  | `failureType` | UInt32 | `static_cast<DWORD>(failure.type)` | Indicates what type of failure was observed (exception, returned error, logged error or fail fast). |
  | `message` | WideString | `failure.pszMessage` | Custom message associated with the failure (if any). |
  | `threadId` | UInt32 | `failure.threadId` | Identifier of the thread the error occurred on. |
  | `callContext` | String | `failure.pszCallContext` | List of telemetry activities containing this error. |
  | `originatingContextId` | UInt32 | `failure.callContextOriginating.contextId` | Identifier for the oldest telemetry activity containing this error. |
  | `originatingContextName` | String | `failure.callContextOriginating.contextName` | Name of the oldest telemetry activity containing this error. |
  | `originatingContextMessage` | WideString | `failure.callContextOriginating.contextMessage` | Custom message associated with the oldest telemetry activity containing this error (if any). |
  | `currentContextId` | UInt32 | `failure.callContextCurrent.contextId` | Identifier for the newest telemetry activity containing this error. |
  | `currentContextName` | String | `failure.callContextCurrent.contextName` | Name of the newest telemetry activity containing this error. |
  | `currentContextMessage` | WideString | `failure.callContextCurrent.contextMessage` | Custom message associated with the newest telemetry activity containing this error (if any). |

---

## Providers with no telemetry events

These providers are defined and registered but emit no telemetry-keyword events. They are listed here for completeness; their (diagnostic) events are out of scope.

| Provider | GUID | Defined in |
|---|---|---|
| `g_hTerminalControlProvider` (`Microsoft.Windows.Terminal.Control`) | `{28c82e50-57af-5a86-c25b-e39cd990032b}` | [`src\cascadia\TerminalControl\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp) |
| `g_UiaProviderTraceProvider` (`Microsoft.Windows.Console.UIA`) | `{e7ebce59-2161-572d-b263-2f16a6afb9e5}` | [`src\types\UiaTracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/types/UiaTracing.cpp) |
| `g_hConsoleVirtTermParserEventTraceProvider` (`Microsoft.Windows.Console.VirtualTerminal.Parser`) | `{c9ba2a84-d3ca-5e19-2bd6-776a0910cb9d}` | [`src\terminal\parser\tracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/terminal/parser/tracing.cpp) |

> `g_hConhostV2EventTraceProvider` is also used for many diagnostic events; only its telemetry-keyword events are listed above. `g_hTerminalControlProvider` has no `TraceLoggingWrite` calls at all in the source tree but receives the cross-provider `FallbackError` event when its DLL is the most recent caller of `EnableFallbackFailureReporting`.
