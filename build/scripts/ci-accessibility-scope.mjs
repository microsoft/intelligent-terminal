// Shared by PR CI publication and accessibility admission; no inference here.
export function relevantPath(path) {
  return /^src\/cascadia\/(TerminalApp|TerminalControl|TerminalSettingsEditor|WindowsTerminal|WindowsTerminal_UIATests|UIMarkdown|LocalTests_TerminalApp\/TestHostApp)\//.test(path) ||
    /(^|\/)\.github\//.test(path) || /^test\/accessibility\//.test(path) ||
    path === 'build/scripts/Get-DependenciesFromAppxRecipe.ps1' ||
    /(^|\/)(AGENTS\.md|CLAUDE\.md|GEMINI\.md|copilot-instructions\.md|SKILL\.md|[^/]+\.instructions\.md|[^/]+\.agent\.md|\.mcp\.json)$/.test(path) ||
    /(^|\/)\.(claude|copilot|agents|gemini)\//.test(path);
}
