# Sond: VS Code syntax highlighting

A TextMate grammar that highlights the Sond programming language (`.snd`, `.sond`):
keywords, contracts (`requires`/`ensures`/`effects`), word operators (`and`/`or`/`implies`/`in`),
booleans, primitive and capability types (`Cap*`), location annotations (`@server`/`@client`),
function names, `//` and `/* */` comments, `"..."` and `"""..."""` strings, integers and decimals.

## Install from VSIX

1. Build the package (produces `sond-<version>.vsix`):
   ```sh
   cd tools/vscode-sond
   npx --yes @vscode/vsce package
   ```
   (Or use a prebuilt `.vsix` if one is shipped in this folder.)
2. In VS Code: open the Extensions view → `...` menu → **Install from VSIX...** → pick the `.vsix`.
   Or from the command line: `code --install-extension sond-0.3.0.vsix`.

## Develop

Open this folder in VS Code and press `F5` to launch an Extension Development Host with the grammar loaded.

## File icon for `.sond` (optional)

The extension ships a **file icon theme** ("Sond") that shows a distinctive icon for `.snd`/`.sond`
files in the Explorer.

Honest caveat: VS Code only supports per-extension file icons via a *file icon theme* that the user
activates, and it **replaces the currently active icon theme** (so other files use this theme's generic
icons). Enable it via: `Preferences: File Icon Theme` → **Sond**. It is opt-in; the syntax highlighting
works regardless.
