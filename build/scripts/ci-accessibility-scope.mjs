// Shared by PR CI publication and accessibility admission; no inference here.
export function relevantPath(path) {
  return /^src\/cascadia\/(TerminalApp|TerminalControl|TerminalSettingsEditor|WindowsTerminal|WindowsTerminal_UIATests|UIMarkdown|LocalTests_TerminalApp\/TestHostApp)\//.test(path) ||
    /(^|\/)\.github\//.test(path) || /^test\/accessibility\//.test(path) ||
    path === 'build/scripts/Get-DependenciesFromAppxRecipe.ps1' ||
    ['build/scripts/ci-accessibility-payload.py', 'build/scripts/ci-accessibility-scope.mjs',
      'build/scripts/test_ci_accessibility_payload.py'].includes(path) ||
    /(^|\/)(AGENTS\.md|CLAUDE\.md|GEMINI\.md|copilot-instructions\.md|SKILL\.md|[^/]+\.instructions\.md|[^/]+\.agent\.md|\.mcp\.json)$/.test(path) ||
    /(^|\/)\.(claude|copilot|agents|gemini)\//.test(path);
}
