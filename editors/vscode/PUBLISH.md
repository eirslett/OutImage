# Publishing the Simula VS Code extension

Guide for shipping `vscode-simula` to Open VSX and/or the Visual Studio Marketplace.

## Prerequisites

- [vsce](https://github.com/microsoft/vscode-vsce) (`npm i -g @vscode/vsce`)
- Publisher account ([Visual Studio Marketplace](https://marketplace.visualstudio.com/manage) / [Open VSX](https://open-vsx.org))
- 128×128 PNG icon at `icons/simula.png` (required for gallery; add before first public release)
- `CHANGELOG.md` entry for the version being published

## Build a `.vsix`

```bash
cd editors/vscode
npm ci
npm test
npm run package
```

Inspect size: `vsce ls --tree` (target &lt; 5 MB without bundled binary).

## Air-gapped / offline install

1. Copy `vscode-simula-*.vsix` and a `sim` binary built for the target OS.
2. Install VSIX: Extensions → `...` → **Install from VSIX**.
3. Set `simula.languageServerPath` to the absolute path of the binary.

No network access is required at runtime.

## Marketplace checklist

- [ ] Publisher id matches `package.json` `publisher`
- [ ] README screenshots (diagnostics, hover, outline)
- [ ] License MIT visible on marketplace
- [ ] `preview: true` until stable 1.0
- [ ] Tag release in git; attach `.vsix` artifact from CI (optional)

## Open VSX

```bash
npx ovsx publish vscode-simula-0.1.0.vsix -p <OVSX_PAT>
```

## Visual Studio Marketplace

```bash
vsce publish -p <VSCE_PAT>
```

## Bundling strategy

**Current:** users install a native `sim` separately (`simula.languageServerPath` / PATH).

Bundling platform binaries inside the VSIX is **not** planned for 0.x.

## Supply chain

- Commit `package-lock.json`
- Run `npm audit` before release; document accepted risks
- CI runs `npm ci && npm test` on every PR
