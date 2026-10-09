# Reference remediation scripts

The toolkit's **Install only the selected patches** actions (*Apply selected OS patches* /
*Apply selected software patches*) don't install anything themselves. NinjaOne has no per-patch
apply endpoint, so the toolkit runs a script from your NinjaOne automation library on each device
and passes it the patches ticked on that device. The two scripts here are reference
implementations of that contract. Review them, import them and point the toolkit at them.

| Script | Action | What it does |
|---|---|---|
| [`Install-SelectedWindowsUpdates.ps1`](./Install-SelectedWindowsUpdates.ps1) | Apply selected OS patches | Uses the Windows Update Agent (`Microsoft.Update.Session`) to install **only** the listed KBs. |
| [`Install-SelectedSoftwarePatches.ps1`](./Install-SelectedSoftwarePatches.ps1) | Apply selected software patches | Decodes and validates the product list and reports it. **It installs nothing until you implement its install hook**, and exits `10` until then. See [below](#the-software-script-needs-an-install-mechanism). |

Both scripts need Windows PowerShell 5.1 or later, contain only ASCII, and make no network
requests of their own. The only downloads come from the Windows Update Agent, from the update
service the device already uses (Microsoft Update or WSUS).

## Importing into NinjaOne

NinjaOne has no API for uploading scripts, so add each one by hand under **Administration →
Library → Automation → Add → New script**:

| Field | Value |
|---|---|
| Name | e.g. `Install-SelectedWindowsUpdates` |
| Language | PowerShell |
| Operating system | Windows |
| Architecture | All (64-bit recommended; the Windows Update Agent is the same on both) |
| Run as | System. The toolkit sends its own **Run as** (Settings → Patch actions, default `system`) with every dispatch. Installing updates needs an elevated context, and both scripts exit `5` when they aren't elevated. |
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
| `productAllowListB64` | Standard base64 (RFC 4648 alphabet `A–Z a–z 0–9 + /`, with `=` padding, not URL-safe) of the **UTF-8** bytes of the selected patches' NinjaOne titles joined by `\|`, e.g. `Google Chrome 141.0.7390.55\|7-Zip`. Titles are NinjaOne's own strings, including the version, and are passed through verbatim. The list is encoded because NinjaOne splits `parameters` on spaces. A title that itself contains `\|` can't be told apart from two titles. |
| `rebootBehavior` | `Never` or `Auto` (`RebootChoice::script_value`). `Auto` schedules a restart 60 seconds after the script finishes, but only when an install asks for one. The delay lets the NinjaOne agent report the result first. `Never` never restarts the device. |
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
| `4` | Installed, but a restart is needed and `rebootBehavior=Never` suppressed it. NinjaOne will probably show the run as failed so that the pending restart is visible. |
| `5` | Environment error: the Windows Update Agent is unavailable, the search failed, another install is in progress, the session isn't elevated, or the software install hook threw an error. |
| `10` | Software script only: no install mechanism is configured, so nothing was installed. |

The OS script writes one line per KB, for example `WOULD INSTALL KB5040434 : '…'`,
`INSTALLED KB5040434 : Succeeded (HRESULT 0x00000000, reboot required) '…'`,
`INSTALLED KB5041580 : already installed`, `NOT FOUND KB5041585 : not offered …`,
`SKIPPED KB… : … is hidden` and `FAILED KB… : …`. It ends with `Result: exit N`.

## The software script needs an install mechanism

NinjaOne's third-party patching is carried out by the NinjaOne agent itself. There is no
supported local command for "install NinjaOne third-party patch *title*", and there are two
further obstacles:

- The WinGet CLI [is not supported as SYSTEM](https://learn.microsoft.com/windows/package-manager/winget/troubleshooting#system-context).
- Mapping a title such as `Google Chrome 141.0.7390.55` to a package id is a decision each tenant
  has to make.

So `Install-SelectedSoftwarePatches.ps1` handles everything that's the same for every tenant:
decoding, validation, per-product reporting, restarts and exit codes. It hands each title to
`Install-SelectedProduct`, a clearly marked function whose shipped version returns
`NotImplemented`. Until you replace it, every run, including a dry run, exits `10`, so the Jobs
tab never shows a success for an install that didn't happen.

To make the script install something, replace `Install-SelectedProduct` with a mapping you have
reviewed, for example with the `Microsoft.WinGet.Client` module (supported as SYSTEM for
machine-wide packages) or Chocolatey. The function's comment spells out the return contract.
Map titles through an explicit table, never a fuzzy search, return `NotOffered` for any title the
table doesn't name, and change nothing when `-DryRun` is set.

## Security

- **These scripts run as SYSTEM on every device you dispatch to.** Read them before importing
  them, and review any change to them the way you would review a change to production. Pin the
  toolkit to the script **id**, not a name, so a script with the same name can't be picked by
  mistake.
- The OS script only ever installs updates whose `KBArticleIDs` include a listed KB. It doesn't
  un-hide updates, change Windows Update settings, or install anything that isn't listed.
- Neither script downloads or runs code from the network, writes credentials anywhere, or reads
  any input other than the four keys above.

## Tests

[`tests/fixtures/parameter-contract.json`](./tests/fixtures/parameter-contract.json) pins the
contract from both sides:

- The Rust test `actions::tests::build_parameters_matches_the_reference_script_fixture` (run by
  `just test`) asserts that `build_parameters` produces each case's `parameters`.
- The Pester 5 suites in [`tests/`](./tests) assert that the scripts parse those same strings
  into each case's `expect`. They also cover rejected input, environment fallback and exit-code
  folding, and check that the two scripts' copies of the parser are identical.

Run the Pester suites on any machine with PowerShell 7 or Windows PowerShell 5.1 and Pester 5:

```powershell
Invoke-Pester -Path remediation/tests -Output Detailed
```

The suites dot-source the scripts, which only defines their functions. They never call the
Windows Update Agent, so they also run on Linux and macOS under `pwsh`.
