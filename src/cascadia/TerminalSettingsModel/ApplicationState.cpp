// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ApplicationState.h"
#include "CascadiaSettings.h"
#include "ApplicationState.g.cpp"
#include "WindowLayout.g.cpp"
#include "ActionAndArgs.h"
#include "JsonUtils.h"
#include "FileUtils.h"
#include "../../types/inc/utils.hpp"

#include <til/io.h>

static constexpr std::wstring_view stateFileName{ L"state.json" };
static constexpr std::wstring_view elevatedStateFileName{ L"elevated-state.json" };

static constexpr std::string_view TabLayoutKey{ "tabLayout" };
static constexpr std::string_view InitialPositionKey{ "initialPosition" };
static constexpr std::string_view InitialSizeKey{ "initialSize" };
static constexpr std::string_view LaunchModeKey{ "launchMode" };

namespace Microsoft::Terminal::Settings::Model::JsonUtils
{
    using namespace winrt::Microsoft::Terminal::Settings::Model;

    template<>
    struct ConversionTrait<WindowLayout>
    {
        WindowLayout FromJson(const Json::Value& json)
        {
            auto layout = winrt::make_self<implementation::WindowLayout>();

            GetValueForKey(json, TabLayoutKey, layout->_TabLayout);
            GetValueForKey(json, InitialPositionKey, layout->_InitialPosition);
            GetValueForKey(json, LaunchModeKey, layout->_LaunchMode);
            GetValueForKey(json, InitialSizeKey, layout->_InitialSize);

            return *layout;
        }

        bool CanConvert(const Json::Value& json)
        {
            return json.isObject();
        }

        Json::Value ToJson(const WindowLayout& val)
        {
            Json::Value json{ Json::objectValue };

            SetValueForKey(json, TabLayoutKey, val.TabLayout());
            SetValueForKey(json, InitialPositionKey, val.InitialPosition());
            SetValueForKey(json, LaunchModeKey, val.LaunchMode());
            SetValueForKey(json, InitialSizeKey, val.InitialSize());

            return json;
        }

        std::string TypeDescription() const
        {
            return "WindowLayout";
        }
    };
}

using namespace ::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    winrt::hstring WindowLayout::ToJson(const Model::WindowLayout& layout)
    {
        JsonUtils::ConversionTrait<Model::WindowLayout> trait;
        auto json = trait.ToJson(layout);

        Json::StreamWriterBuilder wbuilder;
        const auto content = Json::writeString(wbuilder, json);
        return hstring{ til::u8u16(content) };
    }

    Model::WindowLayout WindowLayout::FromJson(const hstring& str)
    {
        auto data = til::u16u8(str);
        std::string errs;
        std::unique_ptr<Json::CharReader> reader{ Json::CharReaderBuilder{}.newCharReader() };

        Json::Value root;
        if (!reader->parse(data.data(), data.data() + data.size(), &root, &errs))
        {
            throw winrt::hresult_error(WEB_E_INVALID_JSON_STRING, winrt::to_hstring(errs));
        }
        JsonUtils::ConversionTrait<Model::WindowLayout> trait;
        return trait.FromJson(root);
    }

    ApplicationState::ApplicationState(const std::filesystem::path& stateRoot) noexcept :
        _sharedPath{ stateRoot / stateFileName },
        _elevatedPath{ stateRoot / elevatedStateFileName },
        _throttler{
            til::throttled_func_options{
                .delay = std::chrono::seconds{ 1 },
                .debounce = true,
                .trailing = true,
            },
            [this]() { _write(); }
        }
    {
        _read();
    }

    // The destructor ensures that the last write is flushed to disk before returning.
    ApplicationState::~ApplicationState()
    {
        Flush();
    }

    void ApplicationState::Flush()
    {
        // This will ensure that we not just cancel the last outstanding timer,
        // but instead force it to run as soon as possible and wait for it to complete.
        const std::scoped_lock lock{ _throttlerMutex };
        _throttler.flush();
    }

    void ApplicationState::_scheduleWrite()
    {
        const std::scoped_lock lock{ _throttlerMutex };
        _throttler();
    }

    // Method Description:
    // - See GH#11119. Removes all of the data in this ApplicationState object
    //   and resets it to the defaults. This will delete the state file! That's
    //   the sure-fire way to make sure the data doesn't come back. If we leave
    //   it untouched, then when we go to write the file back out, we'll first
    //   re-read its contents and try to overlay our new state. However,
    //   nullopts won't remove keys from the JSON, so we'll end up with the
    //   original state in the file.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void ApplicationState::Reset() noexcept
    try
    {
        LOG_LAST_ERROR_IF(!DeleteFile(_sharedPath.c_str()));
        LOG_LAST_ERROR_IF(!DeleteFile(_elevatedPath.c_str()));
        *_state.lock() = {};
    }
    CATCH_LOG()

    // Deserializes the state.json and user-state (or elevated-state if
    // elevated) into this ApplicationState.
    // * ANY errors during app state will result in the creation of a new empty state.
    // * ANY errors during runtime will result in changes being partially ignored.
    void ApplicationState::_read() const noexcept
    try
    {
        std::string errs;
        std::unique_ptr<Json::CharReader> reader{ Json::CharReaderBuilder{}.newCharReader() };

        // First get shared state out of `state.json`.
        const auto sharedData = _readSharedContents();
        if (!sharedData.empty())
        {
            Json::Value root;
            if (!reader->parse(sharedData.data(), sharedData.data() + sharedData.size(), &root, &errs))
            {
                throw winrt::hresult_error(WEB_E_INVALID_JSON_STRING, winrt::to_hstring(errs));
            }

            // - If we're elevated, we want to only load the Shared properties
            //   from state.json. We'll then load the Local props from
            //   `elevated-state.json`
            // - If we're unelevated, then load _everything_ from state.json.
            if (::Microsoft::Console::Utils::IsRunningElevated())
            {
                // Only load shared properties if we're elevated
                FromJson(root, FileSource::Shared);

                // Then, try and get anything in elevated-state
                if (const auto localData{ _readLocalContents() }; !localData.empty())
                {
                    Json::Value root;
                    if (!reader->parse(localData.data(), localData.data() + localData.size(), &root, &errs))
                    {
                        throw winrt::hresult_error(WEB_E_INVALID_JSON_STRING, winrt::to_hstring(errs));
                    }
                    FromJson(root, FileSource::Local);
                }
            }
            else
            {
                // If we're unelevated, then load everything.
                FromJson(root, FileSource::Shared | FileSource::Local);
            }
        }
    }
    CATCH_LOG()

    // Serialized this ApplicationState (in `context`) into the state.json at _path.
    // * Errors are only logged.
    // * _state->_writeScheduled is set to false, signaling our
    //   setters that _synchronize() needs to be called again.
    void ApplicationState::_write() const noexcept
    {
        try
        {
            Json::StreamWriterBuilder wbuilder;

            // When we're elevated, we've got to be tricky. We don't want to write
            // our window state, allowed commandlines, and other Local properties
            // into the shared `state.json`. But, if we only serialize the Shared
            // properties to a json blob, then we'll omit windowState entirely,
            // _removing_ the window state of the unelevated instance. Oh no!
            //
            // So, to be tricky, we'll first _load_ the shared state to a json blob.
            // We'll then serialize our view of the shared properties on top of that
            // blob. Then we'll write that blob back to the file. This will
            // round-trip the Local properties for the unelevated instances
            // untouched in state.json
            //
            // After that's done, we'll write our Local properties into
            // elevated-state.json.
            if (::Microsoft::Console::Utils::IsRunningElevated())
            {
                std::string errs;
                std::unique_ptr<Json::CharReader> reader{ Json::CharReaderBuilder{}.newCharReader() };
                Json::Value root;

                // First load the contents of state.json into a json blob. This will
                // contain the Shared properties and the unelevated instance's Local
                // properties.
                const auto sharedData = _readSharedContents();
                if (!sharedData.empty())
                {
                    if (!reader->parse(sharedData.data(), sharedData.data() + sharedData.size(), &root, &errs))
                    {
                        throw winrt::hresult_error(WEB_E_INVALID_JSON_STRING, winrt::to_hstring(errs));
                    }
                }
                _writeSharedContents(Json::writeString(wbuilder, _toJsonWithBlob(root, FileSource::Shared)));
                _writeLocalContents(Json::writeString(wbuilder, ToJson(FileSource::Local)));
            }
            else
            {
                _writeLocalContents(Json::writeString(wbuilder, ToJson(FileSource::Local | FileSource::Shared)));
            }
            _lastWriteSucceeded.store(true, std::memory_order_release);
        }
        catch (...)
        {
            _lastWriteSucceeded.store(false, std::memory_order_release);
            LOG_CAUGHT_EXCEPTION();
        }
    }

    // Returns the application-global ApplicationState object.
    Microsoft::Terminal::Settings::Model::ApplicationState ApplicationState::SharedInstance()
    {
        static auto state = winrt::make_self<ApplicationState>(GetBaseSettingsPath());
        return *state;
    }

// Need the COMMA macro hack for IMap template arguments in the macros
#define COMMA ,

    // Method Description:
    // - Loads data from the given json blob. Will only read the data that's in
    //   the specified parseSource - so if we're reading the Local state file,
    //   we won't destroy previously parsed Shared data.
    // - READ: there's no layering for app state.
    void ApplicationState::FromJson(const Json::Value& root, FileSource parseSource) const noexcept
    {
        auto state = _state.lock();
        // GetValueForKey() comes in two variants:
        // * take a std::optional<T> reference
        // * return std::optional<T> by value
        // At the time of writing the former version skips missing fields in the json,
        // but we want to explicitly clear state fields that were removed from state.json.
        //
        // GH#11222: We only load properties that are of the same type (Local or
        // Shared) which we requested. If we didn't want to load this type of
        // property, just skip it.
#define MTSM_APPLICATION_STATE_GEN(source, type, name, key, ...) \
    if (WI_IsFlagSet(parseSource, source))                       \
        state->name = JsonUtils::GetValueForKey<std::optional<type>>(root, key);

        MTSM_APPLICATION_STATE_FIELDS(MTSM_APPLICATION_STATE_GEN)
#undef MTSM_APPLICATION_STATE_GEN
    }

    Json::Value ApplicationState::ToJson(FileSource parseSource) const noexcept
    {
        Json::Value root{ Json::objectValue };
        return _toJsonWithBlob(root, parseSource);
    }

    Json::Value ApplicationState::_toJsonWithBlob(Json::Value& root, FileSource parseSource) const noexcept
    {
        {
            const auto state = _state.lock_shared();

            // GH#11222: We only write properties that are of the same type (Local
            // or Shared) which we requested. If we didn't want to serialize this
            // type of property, just skip it.
#define MTSM_APPLICATION_STATE_GEN(source, type, name, key, ...) \
    if (WI_IsFlagSet(parseSource, source))                       \
        JsonUtils::SetValueForKey(root, key, state->name);

            MTSM_APPLICATION_STATE_FIELDS(MTSM_APPLICATION_STATE_GEN)
#undef MTSM_APPLICATION_STATE_GEN
        }
        return root;
    }

#undef COMMA

    void ApplicationState::AppendPersistedWindowLayout(Model::WindowLayout layout)
    {
        {
            const auto state = _state.lock();
            if (!state->PersistedWindowLayouts || !*state->PersistedWindowLayouts)
            {
                state->PersistedWindowLayouts = winrt::single_threaded_vector<Model::WindowLayout>();
            }
            state->PersistedWindowLayouts->Append(std::move(layout));
        }

        _scheduleWrite();
    }

    bool ApplicationState::DismissBadge(const hstring& badgeId)
    {
        bool inserted{ false };
        {
            const auto state = _state.lock();
            if (!state->DismissedBadges)
            {
                state->DismissedBadges = std::unordered_set<hstring>{};
            }
            inserted = state->DismissedBadges->insert(badgeId).second;
        }
        _scheduleWrite();
        return inserted;
    }

    bool ApplicationState::BadgeDismissed(const hstring& badgeId) const
    {
        const auto state = _state.lock_shared();
        if (state->DismissedBadges)
        {
            return state->DismissedBadges->contains(badgeId);
        }
        return false;
    }

    static std::unordered_set<std::wstring> _workspaceBufferFilenames(const Model::WindowLayout& layout)
    {
        std::unordered_set<std::wstring> filenames;
        if (!layout || !layout.TabLayout())
        {
            return filenames;
        }

        for (const auto& action : layout.TabLayout())
        {
            Model::INewContentArgs contentArgs{ nullptr };
            if (const auto args = action.Args().try_as<Model::NewTabArgs>())
            {
                contentArgs = args.ContentArgs();
            }
            else if (const auto args = action.Args().try_as<Model::SplitPaneArgs>())
            {
                contentArgs = args.ContentArgs();
            }

            if (const auto terminalArgs = contentArgs.try_as<Model::NewTerminalArgs>())
            {
                if (const auto sessionId = terminalArgs.SessionId(); sessionId != winrt::guid{})
                {
                    filenames.emplace(fmt::format(FMT_COMPILE(L"workspace_buffer_{}.txt"), sessionId));
                    filenames.emplace(fmt::format(FMT_COMPILE(L"workspace_elevated_{}.txt"), sessionId));
                }
            }
        }
        return filenames;
    }

    static std::unordered_set<std::wstring> _allWorkspaceBufferFilenames(
        const Windows::Foundation::Collections::IMap<hstring, Model::WindowLayout>& workspaces)
    {
        std::unordered_set<std::wstring> filenames;
        if (workspaces)
        {
            for (const auto& pair : workspaces)
            {
                const auto layoutFilenames = _workspaceBufferFilenames(pair.Value());
                filenames.insert(layoutFilenames.begin(), layoutFilenames.end());
            }
        }
        return filenames;
    }

    static void _removeWorkspaceBuffers(const std::filesystem::path& stateRoot,
                                        const Model::WindowLayout& layout,
                                        const std::unordered_set<std::wstring>& keepFilenames = {})
    {
        const auto filenames = _workspaceBufferFilenames(layout);
        for (const auto& filename : filenames)
        {
            if (!keepFilenames.contains(filename))
            {
                std::error_code error;
                std::filesystem::remove(stateRoot / filename, error);
            }
        }
    }

    void ApplicationState::SaveWorkspace(const hstring& name, const Model::WindowLayout& layout)
    {
        LOG_IF_FAILED(SaveWorkspaceAndFlush(name, layout) ? S_OK : E_FAIL);
    }

    bool ApplicationState::SaveWorkspaceAndFlush(const hstring& name, const Model::WindowLayout& layout)
    {
        const std::scoped_lock throttlerLock{ _throttlerMutex };
        Model::WindowLayout oldLayout{ nullptr };
        bool hadOldLayout{ false };
        {
            const auto state = _state.lock();
            if (!state->PersistedWorkspaces || !*state->PersistedWorkspaces)
            {
                state->PersistedWorkspaces = winrt::single_threaded_map<hstring, Model::WindowLayout>();
            }
            const auto map = *state->PersistedWorkspaces;
            if (map.HasKey(name))
            {
                oldLayout = map.Lookup(name);
                hadOldLayout = true;
            }
            map.Insert(name, layout);
        }

        _lastWriteSucceeded.store(false, std::memory_order_release);
        _throttler();
        _throttler.flush();
        if (!_lastWriteSucceeded.load(std::memory_order_acquire))
        {
            {
                const auto state = _state.lock();
                const auto map = *state->PersistedWorkspaces;
                if (map.HasKey(name) && map.Lookup(name) == layout)
                {
                    if (hadOldLayout)
                    {
                        map.Insert(name, oldLayout);
                    }
                    else
                    {
                        map.Remove(name);
                    }
                }
            }
            _lastWriteSucceeded.store(false, std::memory_order_release);
            _throttler();
            _throttler.flush();
            return false;
        }

        std::unordered_set<std::wstring> keepFilenames;
        {
            const auto state = _state.lock_shared();
            if (state->PersistedWorkspaces && *state->PersistedWorkspaces)
            {
                keepFilenames = _allWorkspaceBufferFilenames(*state->PersistedWorkspaces);
            }
        }
        _removeWorkspaceBuffers(_sharedPath.parent_path(), oldLayout, keepFilenames);
        return true;
    }

    bool ApplicationState::RemoveWorkspace(const hstring& name)
    {
        const std::scoped_lock throttlerLock{ _throttlerMutex };
        bool removed{ false };
        Model::WindowLayout removedLayout{ nullptr };
        std::unordered_set<std::wstring> keepFilenames;
        {
            const auto state = _state.lock();
            if (state->PersistedWorkspaces && *state->PersistedWorkspaces)
            {
                auto map = *state->PersistedWorkspaces;
                if (map.HasKey(name))
                {
                    removedLayout = map.Lookup(name);
                    map.Remove(name);
                    keepFilenames = _allWorkspaceBufferFilenames(map);
                    removed = true;
                }
            }
        }
        if (removed)
        {
            _lastWriteSucceeded.store(false, std::memory_order_release);
            _throttler();
            _throttler.flush();
            if (!_lastWriteSucceeded.load(std::memory_order_acquire))
            {
                {
                    const auto state = _state.lock();
                    (*state->PersistedWorkspaces).Insert(name, removedLayout);
                }
                _throttler();
                _throttler.flush();
                return false;
            }
            _removeWorkspaceBuffers(_sharedPath.parent_path(), removedLayout, keepFilenames);
        }
        return removed;
    }

    // Method Description:
    // - Rename a persisted workspace entry from oldName to newName. If there
    //   was no entry for oldName, this is a no-op. If an entry for newName
    //   already exists, it will be overwritten with the layout from oldName.
    // - If newName is empty, the entry under oldName is simply removed (the
    //   old name no longer points at a valid window, so the persisted layout
    //   would otherwise be left stranded).
    // Return Value:
    // - true if the persisted state was modified, false otherwise.
    bool ApplicationState::RenameWorkspace(const hstring& oldName, const hstring& newName)
    {
        if (oldName == newName || oldName.empty())
        {
            return false;
        }

        const std::scoped_lock throttlerLock{ _throttlerMutex };
        bool changed{ false };
        bool hadOverwrittenLayout{ false };
        Model::WindowLayout removedLayout{ nullptr };
        Model::WindowLayout overwrittenLayout{ nullptr };
        std::unordered_set<std::wstring> keepFilenames;
        {
            const auto state = _state.lock();
            if (state->PersistedWorkspaces && *state->PersistedWorkspaces)
            {
                auto map = *state->PersistedWorkspaces;
                if (map.HasKey(oldName))
                {
                    removedLayout = map.Lookup(oldName);
                    if (!newName.empty())
                    {
                        if (map.HasKey(newName))
                        {
                            overwrittenLayout = map.Lookup(newName);
                            hadOverwrittenLayout = true;
                        }
                        map.Insert(newName, removedLayout);
                    }
                    map.Remove(oldName);
                    keepFilenames = _allWorkspaceBufferFilenames(map);
                    changed = true;
                }
            }
        }
        if (changed)
        {
            _lastWriteSucceeded.store(false, std::memory_order_release);
            _throttler();
            _throttler.flush();
            if (!_lastWriteSucceeded.load(std::memory_order_acquire))
            {
                {
                    const auto state = _state.lock();
                    const auto map = *state->PersistedWorkspaces;
                    map.Insert(oldName, removedLayout);
                    if (!newName.empty())
                    {
                        if (hadOverwrittenLayout)
                        {
                            map.Insert(newName, overwrittenLayout);
                        }
                        else if (map.HasKey(newName))
                        {
                            map.Remove(newName);
                        }
                    }
                }
                _throttler();
                _throttler.flush();
                return false;
            }

            if (newName.empty())
            {
                _removeWorkspaceBuffers(_sharedPath.parent_path(), removedLayout, keepFilenames);
            }
            else
            {
                _removeWorkspaceBuffers(_sharedPath.parent_path(), overwrittenLayout, keepFilenames);
            }
        }
        return changed;
    }

    // Method Description:
    // - Atomically remove and return a persisted workspace entry. This is the
    //   intended API for the startup path that restores a named workspace,
    //   because it guarantees only one caller can claim a given workspace.
    // Return Value:
    // - The layout that was stored under `name`, or nullptr if there was none.
    Model::WindowLayout ApplicationState::TakeWorkspace(const hstring& name)
    {
        Model::WindowLayout result{ nullptr };
        {
            const auto state = _state.lock();
            if (state->PersistedWorkspaces && *state->PersistedWorkspaces)
            {
                auto map = *state->PersistedWorkspaces;
                if (map.HasKey(name))
                {
                    result = map.Lookup(name);
                    map.Remove(name);
                }
            }
        }
        if (result)
        {
            _scheduleWrite();
        }
        return result;
    }

    Windows::Foundation::Collections::IMapView<hstring, Model::WindowLayout> ApplicationState::AllPersistedWorkspaces()
    {
        const auto state = _state.lock_shared();
        if (state->PersistedWorkspaces && *state->PersistedWorkspaces)
        {
            return (*state->PersistedWorkspaces).GetView();
        }
        return nullptr;
    }

    // Generate all getter/setters
#define MTSM_APPLICATION_STATE_GEN(source, type, name, key, ...) \
    type ApplicationState::name() const noexcept                 \
    {                                                            \
        const auto state = _state.lock_shared();                 \
        const auto& value = state->name;                         \
        return value ? *value : type{ __VA_ARGS__ };             \
    }                                                            \
                                                                 \
    void ApplicationState::name(const type& value) noexcept      \
    {                                                            \
        {                                                        \
            const auto state = _state.lock();                    \
            state->name.emplace(value);                          \
        }                                                        \
                                                                 \
        _scheduleWrite();                                        \
    }
#define COMMA ,
    MTSM_APPLICATION_STATE_FIELDS(MTSM_APPLICATION_STATE_GEN)
#undef COMMA
#undef MTSM_APPLICATION_STATE_GEN

    // Method Description:
    // - Read the contents of our "shared" state - state that should be shared
    //   for elevated and unelevated instances. This is things like the list of
    //   generated profiles, the command palette commandlines.
    std::string ApplicationState::_readSharedContents() const
    {
        return til::io::read_file_as_utf8_string_if_exists(_sharedPath);
    }

    // Method Description:
    // - Read the contents of our "local" state - state that should be kept in
    //   separate files for elevated and unelevated instances. This is things
    //   like the persisted window state, and the approved commandlines (though,
    //   those don't matter when unelevated).
    // - When elevated, this will DELETE `elevated-state.json` if it has bad
    //   permissions, so we don't potentially read malicious data.
    std::string ApplicationState::_readLocalContents() const
    {
        return ::Microsoft::Console::Utils::IsRunningElevated() ?
                   til::io::read_file_as_utf8_string_if_exists(_elevatedPath, true) :
                   til::io::read_file_as_utf8_string_if_exists(_sharedPath, false);
    }

    // Method Description:
    // - Write the contents of our "shared" state - state that should be shared
    //   for elevated and unelevated instances. This will atomically write to
    //   `state.json`
    void ApplicationState::_writeSharedContents(const std::string_view content) const
    {
        til::io::write_utf8_string_to_file_atomic(_sharedPath, content);
    }

    // Method Description:
    // - Write the contents of our "local" state - state that should be kept in
    //   separate files for elevated and unelevated instances. When elevated,
    //   this will write to `elevated-state.json`, and when unelevated, this
    //   will atomically write to `user-state.json`
    void ApplicationState::_writeLocalContents(const std::string_view content) const
    {
        if (::Microsoft::Console::Utils::IsRunningElevated())
        {
            // DON'T use til::io::write_utf8_string_to_file_atomic, which will write to a temporary file
            // then rename that file to the final filename. That actually lets us
            // overwrite the elevate file's contents even when unelevated, because
            // we're effectively deleting the original file, then renaming a
            // different file in its place.
            //
            // We're not worried about someone else doing that though, if they do
            // that with the wrong permissions, then we'll just ignore the file and
            // start over.
            til::io::write_utf8_string_to_file(_elevatedPath, content, true);
        }
        else
        {
            til::io::write_utf8_string_to_file_atomic(_sharedPath, content);
        }
    }

}
