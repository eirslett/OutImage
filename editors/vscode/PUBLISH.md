# Publishing the Simula VS Code extension

Guide for shipping `vscode-simula` to Open VSX and/or the Visual Studio Marketplace.

Official GitHub releases (**Actions → Release**) attach `vscode-simula.vsix` and
publish that same VSIX to the Visual Studio Marketplace via Microsoft Entra
workload identity federation (no Azure DevOps PAT).

## Prerequisites

- Publisher id `eirslett` in `package.json` (already created)
- 128×128 PNG icon at `icons/simula.png`
- `CHANGELOG.md` entry for the version being published
- Entra / Azure / GitHub environment setup below (once)

## Visual Studio Marketplace (workload identity federation)

The [VS Code docs](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#secure-automated-publishing-to-visual-studio-marketplace)
describe this for Azure Pipelines. This repo uses the GitHub Actions equivalent:
GitHub OIDC → user-assigned managed identity → `vsce publish --azure-credential`.

Do these once. Values in angle brackets are yours to fill in.

### 1. Managed identity (Azure)

In the [Azure portal](https://portal.azure.com) (or CLI):

1. Create a resource group if you need one (any region).
2. Create a **user-assigned managed identity**, e.g. `outimage-vscode-marketplace`.
3. Assign it **Reader** on the subscription (enough for `azure/login`; Marketplace
   access is granted separately in step 4).
4. From the identity’s **Overview**, copy:

   - **Client ID** → `AZURE_CLIENT_ID`
   - **Tenant ID** (Directory ID) → `AZURE_TENANT_ID`
   - Subscription ID of the subscription that owns the identity → `AZURE_SUBSCRIPTION_ID`

### 2. Federated credential (Azure → GitHub)

On that managed identity: **Settings → Federated credentials → Add**.

- Federated credential scenario: **GitHub Actions deploying Azure resources**
- Organization: `eirslett`
- Repository: `OutImage` (exact casing)
- Entity: **Environment**
- Environment name: `vscode-marketplace`
- Name: anything, e.g. `github-vscode-marketplace`

Save. Issuer should be `https://token.actions.githubusercontent.com` and subject
`repo:eirslett/OutImage:environment:vscode-marketplace`.

### 3. GitHub environment secrets

In the GitHub repo: **Settings → Environments → New environment**

- Name: `vscode-marketplace` (must match the federated credential)
- Environment secrets:

  | Secret | Value |
  | --- | --- |
  | `AZURE_CLIENT_ID` | managed identity client ID |
  | `AZURE_TENANT_ID` | Entra tenant ID |
  | `AZURE_SUBSCRIPTION_ID` | Azure subscription ID |

Optional: add required reviewers so a human must approve Marketplace publishes.

### 4. Add the identity to publisher `eirslett`

1. Merge this workflow, then run **Actions → VS Code Marketplace identity**
   (`workflow_dispatch`).
2. From the log JSON, copy the `id` field (Marketplace / Azure DevOps profile
   GUID — not the Azure client ID).
3. Open [Publisher management](https://marketplace.visualstudio.com/manage/publishers/eirslett)
   while signed in as the publisher owner.
4. **Members → Add**, paste that `id`, role **Contributor**.

If add-by-id fails, the publisher may still be tied to a personal Microsoft
account. Entra identities need an org-backed publisher; you may have to
[create/move the publisher](https://marketplace.visualstudio.com/manage) under
the same Entra tenant as the managed identity.

### 5. Cut a release

**Actions → Release** (`patch` / `minor` / `major`). After the GitHub release
succeeds, **Publish VS Code Marketplace** runs `vsce publish --azure-credential`
on `vscode-simula.vsix`. Re-runs skip an already-published version
(`--skip-duplicate`).

Local equivalent after `az login`:

```bash
cd editors/vscode
npx vsce publish --azure-credential --packagePath path/to/vscode-simula.vsix --no-dependencies
```

## Build a `.vsix` locally

```bash
cd editors/vscode
npm ci
npm test
npm run package
```

Inspect size: `vsce ls --tree` (target &lt; 5 MB without bundled binary).

## Air-gapped / offline install

1. Copy `vscode-simula.vsix` (from the GitHub release) and a `sim` binary for the target OS.
2. Install VSIX: Extensions → `...` → **Install from VSIX**.
3. Set `simula.languageServerPath` to the absolute path of the binary.

No network access is required at runtime.

## Marketplace checklist

- [ ] Publisher id matches `package.json` `publisher`
- [ ] README screenshots (diagnostics, hover, outline)
- [ ] License MIT visible on marketplace
- [ ] `preview: true` until stable 1.0
- [ ] Entra WIF + `vscode-marketplace` GitHub environment configured
- [ ] Cut a GitHub release (**Actions → Release**); Marketplace publish is part of that workflow

## Open VSX

Still uses an Eclipse PAT (not Azure DevOps):

```bash
npx ovsx publish vscode-simula.vsix -p <OVSX_PAT>
```

## Bundling strategy

**Current:** users install a native `sim` separately (`simula.languageServerPath` / PATH).

Bundling platform binaries inside the VSIX is **not** planned for 0.x.

## Supply chain

- Commit `package-lock.json`
- Run `npm audit` before release; document accepted risks
- CI runs `npm ci && npm test` on every PR
