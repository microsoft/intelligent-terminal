@echo off

echo Setting up dev environment...

rem Open Console build environment setup
rem Adds msbuild to your path, and adds the open\tools directory as well
rem This recreates what it's like to be an actual windows developer!

rem skip the setup if we're already ready.
if not "%OpenConBuild%" == "" goto :END

rem Add Opencon build scripts to path
set PATH=%PATH%;%~dp0;

rem add some helper envvars - The Opencon root, and also the processor arch, for output paths
set OPENCON_TOOLS=%~dp0
rem The opencon root is at ...\open\tools\, without the last 7 chars ('\tools\')
set OPENCON=%OPENCON_TOOLS:~0,-7%

rem Add nuget to PATH
set PATH=%OPENCON%\dep\nuget;%PATH%

rem Run nuget restore so you can use vswhere
nuget restore %OPENCON%\OpenConsole.slnx -Verbosity quiet
nuget restore %OPENCON%\dep\nuget\packages.config -Verbosity quiet

:FIND_MSBUILD
set MSBUILD=

rem GH#1313: If msbuild is already on the path, we don't need to look for it.
for %%X in (msbuild.exe) do (set MSBUILD=%%~$PATH:X)
if defined MSBUILD (
    echo Using MSBuild at %MSBUILD% which was already on the path.
    goto :FOUND_MSBUILD
)

rem Find vswhere
rem from https://github.com/microsoft/vs-setup-samples/blob/master/tools/vswhere.cmd
for /f "usebackq delims=" %%I in (`dir /b /aD /o-N /s "%~dp0..\packages\vswhere*" 2^>nul`) do (
    for /f "usebackq delims=" %%J in (`where /r "%%I" vswhere.exe 2^>nul`) do (
        set VSWHERE=%%J
    )
)

if not defined VSWHERE (
    echo Could not find vswhere on your machine. Please set the VSWHERE variable to the location of vswhere.exe and run razzle again.
    exit /b 1
)

rem Add path to MSBuild Binaries
rem
rem We accept the latest prerelease of VS in the 17.x or 18.x range. The -version
rem range [17.0,19.0) picks up both VS 2022 (17.x) and VS 18 (including previews)
rem but not a still-newer major whose toolset may be incompatible. VS 18 uses our
rem v145 PlatformToolset (see src\common.build.pre.props); older VS versions default
rem to v143.
rem
rem NOTE: this intentionally does NOT use `for /f ... in (`command`)` to capture
rem vswhere's output. cmd.exe's parser scans for the closing ')' of a `for /f`
rem block by naive character counting -- it does not understand that the ')' in
rem the -version range "[17.0,19.0)" is inside quotes, so it prematurely closes
rem the `in (...)` clause and truncates the command, silently leaving MSBUILD
rem unset even when a matching VS install exists. Escaping it (^)) or swapping
rem to "[17.0,19.0]" does not help -- the same truncation still happens.
rem Route through a temp file instead, which sidesteps backtick/paren parsing
rem entirely. We then read the file back with `for /f ... in ("file")`
rem (not `in (`command`)`, so no paren-counting issue) and let the loop body
rem overwrite MSBUILD on every line, same as the original `for /f` did --
rem this preserves "last match wins" if -find ever globs multiple paths,
rem instead of the "first line wins" behavior a plain `set /p` would give.
rem
rem We `pushd` into %TEMP% and reference the temp file by a plain relative
rem name (no directory component) so that the text inside `in (...)` never
rem contains the expanded %TEMP% path. If %TEMP% itself contained a ')'
rem (e.g. a profile directory like "C:\Users\Jane (Admin)\AppData\..."),
rem that same naive paren-counting parser bug would truncate the `in (...)`
rem clause again -- this sidesteps it entirely.
rem
rem `pushd` itself can fail (e.g. %TEMP% unset or pointing at a directory
rem that no longer exists), in which case it neither changes directory nor
rem pushes anything onto the directory stack. We still attempt vswhere
rem discovery in that case -- the temp file simply lands in the current
rem directory instead of %TEMP% -- rather than giving up on finding
rem MSBuild entirely just because %TEMP% is misconfigured. We only call
rem `popd` when the matching `pushd` actually succeeded, tracked via
rem VSWHERE_TEMP_PUSHED, so a failed pushd can't pop a preexisting
rem directory stack entry from the caller's shell and yank them to an
rem unrelated directory.
rem
rem NOTE: none of the steps below are wrapped in an `if ( ... )` block.
rem Without `setlocal enabledelayedexpansion`, cmd.exe expands all %VAR%
rem references in a parenthesized block at parse time, before any `set`
rem inside that same block has run -- so VSWHERE_MSBUILD_TMP would still
rem resolve to its prior (unset) value everywhere it's used below. Plain
rem sequential lines sidestep that, since each is parsed and expanded only
rem when reached.
set "VSWHERE_TEMP_PUSHED=0"
pushd "%TEMP%" >nul 2>nul
if not errorlevel 1 set "VSWHERE_TEMP_PUSHED=1"

set "VSWHERE_MSBUILD_TMP=razzle-vswhere-msbuild-%RANDOM%.txt"
"%VSWHERE%" -latest -prerelease -products * -requires Microsoft.Component.MSBuild -version "[17.0,19.0)" -find MSBuild\**\Bin\MSBuild.exe > "%VSWHERE_MSBUILD_TMP%" 2>nul
rem Guard the read: if the temp file never got created (e.g. the
rem vswhere invocation above failed outright), skip the for /f entirely
rem instead of letting it print "The system cannot find the file
rem specified." -- that noise would mask the real "Could not find
rem MSBuild" error reported below.
if exist "%VSWHERE_MSBUILD_TMP%" (
    for /f "usebackq delims=" %%B in ("%VSWHERE_MSBUILD_TMP%") do (set "MSBUILD=%%B")
)
del /q "%VSWHERE_MSBUILD_TMP%" 2>nul
set "VSWHERE_MSBUILD_TMP="
if "%VSWHERE_TEMP_PUSHED%"=="1" popd >nul 2>nul
set "VSWHERE_TEMP_PUSHED="

if not defined MSBUILD (
    echo Could not find MSBuild on your machine. Please set the MSBUILD variable to the location of MSBuild.exe and run razzle again.
    exit /b 1
)

:FOUND_MSBUILD

rem Guard: make sure we actually resolved a real MSBuild.exe. Without this, a
rem chained command like `razzle && bcz` would run bcz with an empty MSBUILD/
rem PLATFORM/CONFIGURATION and fail cryptically with '""' is not recognized.
if not exist "%MSBUILD%" (
    echo Could not find a usable MSBuild.exe ^(resolved: "%MSBUILD%"^).
    echo Open a "Developer PowerShell/Command Prompt for VS", or run
    echo   Import-Module .\tools\OpenConsole.psm1; Set-MsbuildDevEnvironment
    echo in your shell before razzle, then try again.
    exit /b 1
)

rem Add MSBuild's own directory to PATH, with a proper ; separator.
for %%F in ("%MSBUILD%") do set "MSBUILD_BIN=%%~dpF"
set "PATH=%PATH%;%MSBUILD_BIN%"

if "%PROCESSOR_ARCHITECTURE%" == "AMD64" (
    set ARCH=x64
    set PLATFORM=x64
) else (
    set ARCH=x86
    set PLATFORM=Win32
)
set DEFAULT_CONFIGURATION=Debug

rem call .razzlerc - for your generic razzle environment stuff
if exist "%OPENCON_TOOLS%\.razzlerc.cmd" (
    call %OPENCON_TOOLS%\.razzlerc.cmd
)   else (
    (
        echo @echo off
        echo.
        echo rem This is your razzlerc file. It can be used for default dev environment setup.
    ) > %OPENCON_TOOLS%\.razzlerc.cmd
)

rem if there are args, run them. This can be used for additional env. customization,
rem    especially on a per shortcut basis.
:ARGS_LOOP
if (%1) == () goto :POST_ARGS_LOOP
if (%1) == (dbg) (
    set DEFAULT_CONFIGURATION=Debug
    shift
    goto :ARGS_LOOP
)
if (%1) == (rel) (
    set DEFAULT_CONFIGURATION=Release
    shift
    goto :ARGS_LOOP
)
if (%1) == (x86) (
    set ARCH=x86
    set PLATFORM=Win32
    shift
    goto :ARGS_LOOP
)
if exist %1 (
    call %1
) else (
    echo Could not locate "%1"
)
shift
goto :ARGS_LOOP

:POST_ARGS_LOOP
set TAEF=%OPENCON%\packages\Microsoft.Taef.10.100.251104001\build\Binaries\%ARCH%\TE.exe
rem Set this envvar so setup won't repeat itself
set OpenConBuild=true

:END
echo The dev environment is ready to go!
exit /b 0
