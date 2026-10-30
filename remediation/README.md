# Reference remediation scripts

The toolkit's **Apply selected** actions (*Apply selected OS patches* /
*Apply selected software patches*) don't install anything themselves. NinjaOne has no per-patch
apply endpoint, so the toolkit runs a script from your NinjaOne automation library on each device
and passes it the patches ticked on that device. The two scripts here are reference
implementations of that contract. Review them, import them and point the toolkit at them.

| Script | Action | What it does |
|---|---|---|
| [`Install-SelectedWindowsUpdates.ps1`](./Install-SelectedWindowsUpdates.ps1) | Apply selected OS patches | Uses the Windows Update Agent (`Microsoft.Update.Session`) to install **only** the listed KBs. |
| [`Install-SelectedSoftwarePatches.ps1`](./Install-SelectedSoftwarePatches.ps1) | Apply selected software patches | Maps each listed NinjaOne title to a WinGet package through an explicit table and upgrades it with Microsoft's `Microsoft.WinGet.Client` PowerShell module. See [below](#how-the-software-script-installs). |

Both scripts need Windows PowerShell 5.1 or later and contain only ASCII. The OS script makes no
network requests of its own: its only downloads come from the Windows Update Agent, from the
update service the device already uses (Microsoft Update or WSUS). The software script installs
`Microsoft.WinGet.Client` from the PowerShell Gallery when a device lacks it, and WinGet downloads
installers from its configured sources.

## Importing into NinjaOne

NinjaOne has no API for uploading scripts, so add each one by hand under **Administration →
Library → Automation → Add → New script**:

| Field | Value |
|---|---|
| Name | e.g. `Install-SelectedWindowsUpdates` |
| Language | PowerShell |
| Operating system | Windows |
| Architecture | All (64-bit recommended; the Windows Update Agent is the same on both) |
| Run as | System. The toolkit sends its own **Run as** (Settings → Patch actions, default `system`) with every dispatch, so this is only the library default. Installing updates needs an elevated context, and both scripts exit `5` when they aren't elevated. For the software script, see [which Run as to use](#which-run-as-to-use). |
| Script | paste the file's contents unchanged |

Then add these **script variables**. Use exactly these names (case doesn't matter), type
**String**, and leave them overridable:

| Script | Variables |
|---|---|
| `Install-SelectedWindowsUpdates.ps1` | `kbAllowList`, `rebootBehavior`, `dryRun` |
| `Install-SelectedSoftwarePatches.ps1` | `productAllowListB64`, `rebootBehavior`, `dryRun` |

The toolkit reads these declarations from `GET /automation/scripts` to decide what to offer:

- `kbAllowList` turns on per-KB targeting ("Target only the selected KBs") in the generic
  **Run script** picker.
- `dryRun` marks the script as supporting a preview. **Dry run** is only honoured for a script
  that declares it.

If a variable isn't declared, the toolkit treats the script as unable to take that input, even
though these scripts would parse it.

Finally, copy each script's numeric id from its URL in the NinjaOne console into **Settings →
Patch actions → OS remediation script ID** / **Software remediation script ID**. If an id isn't
set, the matching action is blocked. The toolkit reads the ids from Settings, never from the
request.

## The parameter contract

The toolkit composes one parameter string per device in
[`src-tauri/src/actions/parameters.rs::build_parameters`](../src-tauri/src/actions/parameters.rs) and sends it as the
`parameters` of `POST /device/{id}/script/run`:

```text
kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true
productAllowListB64=R29vZ2xlIENocm9tZSAxNDEuMC43MzkwLjU1fDctWmlw rebootBehavior=Auto dryRun=false
```

| Key | Value |
|---|---|
| `kbAllowList` | Comma-separated KB numbers, digits only, with no `KB` prefix and no spaces. The scripts also accept an optional `KB`/`kb` prefix when you run them by hand. Any other character, an empty entry or an empty list makes the script exit `1`. |
| `productAllowListB64` | Standard base64 (RFC 4648 alphabet `A–Z a–z 0–9 + /`, with `=` padding, not URL-safe) of the **UTF-8** bytes of the selected patches' NinjaOne titles joined by `\|`, e.g. `Google Chrome 141.0.7390.55\|7-Zip`. Titles are NinjaOne's own strings, including the version, and are passed through verbatim. The list is encoded because NinjaOne splits `parameters` on spaces. A title that itself contains `\|` can't be told apart from two titles, so the toolkit refuses to send one (the plan is blocked and names it), and it never sends a blank title. |
| `rebootBehavior` | `Never` or `Auto` (`RebootChoice::script_value`). `Auto` schedules a restart 60 seconds after the script finishes whenever a restart is pending at that point. For the OS script that includes a restart that was already pending before the run (it reads Windows Update's pending-restart flag first); the software script restarts only for its own installs. The delay lets the NinjaOne agent report the result first. `Never` never restarts the device. In the app this is **Restart afterwards if a restart is pending**. |
| `dryRun` | `true` or `false`. With `true`, the script only reports what it would install. |

How the scripts read it:

- **Argument line first.** NinjaOne passes the space-split pieces to the script as arguments.
  An argument that PowerShell split on commas is joined back together.
- **Environment second.** Any key missing from the argument line is read from the environment
  variable of the same name, which is how NinjaOne exposes a declared script variable. The
  argument line always wins, and an empty variable counts as missing.
- **Strict.** An unknown key (such as a typo like `dry_run`), a repeated key or a token without
  `=` makes the script exit `1`. The script never falls back to a default in those cases.
- **Safe defaults.** A missing `rebootBehavior` means `Never` and a missing `dryRun` means
  `true`. The toolkit always sends both, so these defaults only apply when you run a script by
  hand.

Each device gets **only its own** ticked patches. A device with nothing of that family ticked is
dropped from the dispatch rather than sent an empty list, and the scripts refuse an empty list
anyway.

## Exit codes

The Jobs tab shows each run's exit code. NinjaOne's v2 API doesn't return script output, so the
per-KB and per-product lines described below are only visible in the NinjaOne console's activity
for that run.

| Code | Meaning |
|---|---|
| `0` | Every listed patch is installed or already was. On a dry run, every listed patch would install or is already installed. If a restart was needed, `rebootBehavior=Auto` scheduled it. |
| `1` | Bad input: the parameters are missing, malformed or empty, or not valid base64/UTF-8. The script changed nothing. |
| `2` | Nothing matched: none of the listed patches is offered to this device or installed on it. |
| `3` | Incomplete: at least one listed patch failed to download or install, is hidden on the device, or wasn't offered while others were. |
| `4` | A restart is pending and `rebootBehavior=Never` held it back. For the OS script the restart may have been pending before this run, so the code can appear even when nothing new was installed. NinjaOne will probably show the run as failed so that the pending restart is visible. |
| `5` | Environment error: the Windows Update Agent is unavailable, the search failed, another install is in progress, or the session isn't elevated. For the software script: `Microsoft.WinGet.Client` couldn't be installed, loaded or run, or isn't signed by Microsoft (the log line says how to fix the device). |
| `10` | Software script only: the install hook reported it has no install mechanism. The shipped hook never does; a replacement hook that returns `NotImplemented` would. |

The OS script writes one line per KB, for example `WOULD INSTALL KB5040434 : '…'`,
`INSTALLED KB5040434 : Succeeded (HRESULT 0x00000000, reboot required) '…'`,
`INSTALLED KB5041580 : already installed`, `NOT FOUND KB5041585 : not offered …`,
`SKIPPED KB… : … is hidden` and `FAILED KB… : …`. It ends with `Result: exit N`.

## How the software script installs

NinjaOne's third-party patching is carried out by the NinjaOne agent itself. There is no
supported local command for "install NinjaOne third-party patch *title*", so the software script
upgrades through the Windows Package Manager instead, with Microsoft's `Microsoft.WinGet.Client`
module. It doesn't use `winget.exe`: the CLI
[isn't supported as SYSTEM](https://learn.microsoft.com/windows/package-manager/winget/troubleshooting#system-context),
and there its column headers come back as resource keys (`SearchName SearchId SearchVersion`)
instead of English. The module returns objects, so the script never parses WinGet's text output.

For each title it:

1. Looks the title up in `Get-ProductPackageMap`, a table near the top of the install hook that
   maps a normalized NinjaOne title (`ChromeEnterprise (X64)` → `chromeenterprisex64`) to a WinGet
   package id (`Google.Chrome`). Versions, spaces and punctuation are dropped before the lookup,
   so `Google Chrome 141.0.7390.55` and `7-Zip 24.08 (x64)` match their rows too. A title that
   isn't in the table is reported `NOT OFFERED`. The log line names the key to add and the
   installed packages that may match, so you can copy the right id into the table. Nothing is
   ever guessed at.
2. Asks the module for that package (`Get-WinGetPackage`, exact id). Not installed →
   `NOT OFFERED`. No newer version → `ALREADY INSTALLED`. An unknown installed version, or the
   same package installed twice, → `FAILED`, because the script can't tell whether the patch
   applies.
3. On a dry run, reports `WOULD INSTALL` with the version change. Otherwise it runs
   `Update-WinGetPackage` silently and reports `INSTALLED` or `FAILED` with WinGet's status and the
   installer's exit code. A restart the installer asks for is handled by `rebootBehavior`.

**Before rolling it out, extend `Get-ProductPackageMap`** with the products you patch. Run a dry
run first: every `NOT OFFERED` line tells you what to add.

### Which Run as to use

`Microsoft.WinGet.Client` works as SYSTEM only in PowerShell 7, and NinjaOne runs scripts in
Windows PowerShell 5.1 (the cmdlets fail there with "This cmdlet is not supported in Windows
PowerShell", [winget-cli#4820](https://github.com/microsoft/winget-cli/issues/4820)). Pick one:

| Run as | Needs on each device | How it works |
|---|---|---|
| **System** (the default) | PowerShell 7 | The script re-runs itself in PowerShell 7 (`pwsh -MTA`). Deploy PowerShell 7 with NinjaOne's own software deployment first, to its default location (`Program Files\PowerShell\7`). The script runs `pwsh.exe` only from there, and only when its Authenticode signature is Microsoft's. It never looks `pwsh.exe` up on `PATH`, because a `PATH` folder that a local user can write would let that user run code as SYSTEM. Without a signed PowerShell 7 there, every run exits `5` and says why. |
| **A Windows credential** (Preferred Windows local admin, Preferred Windows domain admin, or a specific credential) | Nothing extra | Under a real account the module works in Windows PowerShell 5.1, so the script stays there. The account must be an administrator, and NinjaOne must start the run elevated, or a real install exits `5`. WinGet (App Installer) must be registered for that account. If a run fails saying App Installer is missing, run `Repair-WinGetPackageManager -AllUsers -Latest` on the device once. If it fails with error `0x800706BA`, the account usually has no App Installer registration (common for a service account that has never signed in): run `Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe` once as that account. |

The toolkit sends its **Run as** with every dispatch: **Settings → Patch actions** sets the
default, and the action bar can override it for one dispatch. NinjaOne's **Windows 3rd Party
Patching** preferred credential is a policy slot that only NinjaOne's own patch engine reads. It
isn't a script run-as role, so it can't be chosen by role. Choosing the same credential by name
works.

The first run on a device without `Microsoft.WinGet.Client` installs it for all users from the
PowerShell Gallery (plus the NuGet package provider in Windows PowerShell). This happens on a
dry run too. It is the one change a dry run makes, and the log shows it as a `SETUP` line. The
device needs to reach `www.powershellgallery.com` for that first run.

Before every load, whether it was installed just now or earlier, the script checks that
Microsoft signed the module, using the rule PowerShellGet uses for its own publisher check. If
the module ships a catalog (`Microsoft.WinGet.Client.cat`), the catalog must be signed by
Microsoft and every file must still match it. Without a catalog, the manifest
(`Microsoft.WinGet.Client.psd1`) must be signed by Microsoft. Every file that code can load from
(`.psd1`, `.psm1`, `.ps1`, `.ps1xml`, `.cdxml`, `.dll`, `.exe`) must then carry a valid
signature: Microsoft's for the `Microsoft.*` files, the publisher's own for a bundled third-party
library. If any check fails, the run exits `5`, names the file and the problem, and doesn't load
the module.

## Security

- **These scripts run as SYSTEM (or the credential you choose) on every device you dispatch to.** Read them before importing
  them, and review any change to them the way you would review a change to production. Pin the
  toolkit to the script **id**, not a name, so a script with the same name can't be picked by
  mistake.
- The OS script only ever installs updates whose `KBArticleIDs` include a listed KB. It doesn't
  un-hide updates, change Windows Update settings, or install anything that isn't listed.
- The software script only ever upgrades packages that `Get-ProductPackageMap` names, and only
  ones already installed. It never installs a product the device doesn't have.
- Neither script writes credentials anywhere or reads any input other than the four keys above.
  The OS script downloads nothing itself. The software script downloads `Microsoft.WinGet.Client`
  from the PowerShell Gallery when it's missing, and loads it only once its signature checks out
  as Microsoft's (above). WinGet downloads the installers it runs from its configured sources (by
  default Microsoft's `winget` community repository).
- Neither script runs a program by bare name. As SYSTEM, a bare name is looked up on the machine
  `PATH`, which often lists a folder any local user can write (a folder created directly under
  `C:\` is one). `shutdown.exe` is called as `%SystemRoot%\System32\shutdown.exe`, and PowerShell 7
  is used only from `Program Files` with Microsoft's signature on it.

## Tests

[`tests/fixtures/parameter-contract.json`](./tests/fixtures/parameter-contract.json) pins the
contract from both sides:

- The Rust test `actions::tests::build_parameters_matches_the_reference_script_fixture` (run by
  `just test`) asserts that `build_parameters` produces each case's `parameters`.
- The Pester 5 suites in [`tests/`](./tests) assert that the scripts parse those same strings
  into each case's `expect`. They also cover rejected input, environment fallback and exit-code
  folding, and check that the two scripts' copies of the parser are identical.

Run the Pester suites on any machine with PowerShell 7 or Windows PowerShell 5.1 and Pester 5
or later: `just pester` (PowerShell 7) or `just pester powershell` (Windows PowerShell 5.1), or
directly:

```powershell
Invoke-Pester -Path remediation/tests -Output Detailed
```

CI's `pester` job runs them under both interpreters on Windows for every code change.

The suites dot-source the scripts, which only defines their functions. They never call the
Windows Update Agent or WinGet (the software hook's module calls go through thin wrappers that
the suite mocks), so they also run on Linux and macOS under `pwsh`.
