<#
.SYNOPSIS
    Remediation script for the NinjaOne Patch Toolkit's "Apply selected software patches" action.
    Decodes and validates the toolkit's product allow list, maps each NinjaOne product title to a
    winget package id through an explicit table, and upgrades it with Microsoft's
    Microsoft.WinGet.Client PowerShell module.

.DESCRIPTION
    The toolkit dispatches this script through NinjaOne's POST /device/{id}/script/run with a
    parameter string built by src-tauri/src/actions/parameters.rs::build_parameters:

        productAllowListB64=R29vZ2xlIENocm9tZSAxNDEuMC43MzkwLjU1fDctWmlw rebootBehavior=Never dryRun=true

    productAllowListB64 is standard base64 (RFC 4648 alphabet A-Z a-z 0-9 + /, "=" padding) of the
    UTF-8 bytes of the selected patches' NinjaOne titles joined by "|" - here
    "Google Chrome 141.0.7390.55|7-Zip". Titles carry spaces and NinjaOne splits the parameter
    string on spaces, which is why the list travels encoded.

    HOW THE INSTALL IS PERFORMED
    NinjaOne's third-party patching is performed by the NinjaOne agent itself; there is no local,
    supported command that says "install NinjaOne third-party patch <title>". This script upgrades
    through the Windows Package Manager instead, using the Microsoft.WinGet.Client module rather
    than winget.exe: the CLI is not supported as SYSTEM, and there its column headers come back as
    resource keys ("SearchId") that no parser can trust. The module returns objects, so nothing
    here reads winget's text output.

    The module runs as SYSTEM only in PowerShell 7 started with -MTA. NinjaOne runs scripts in
    Windows PowerShell 5.1, so as SYSTEM the script re-runs itself in PowerShell 7 when it is
    installed. Under any other account (Run as a Windows credential) the module works in Windows
    PowerShell 5.1 and the script stays there.

    When the module is missing, the script installs it for all users from the PowerShell Gallery,
    on a dry run too: that is the one change a dry run makes to a device, and the log says so.

    A NinjaOne title ("ChromeEnterprise (X64)") is not a winget package id ("Google.Chrome"), so the
    mapping is an explicit table - Get-ProductPackageMap - rather than a fuzzy search. A title the
    table does not list is reported NotOffered, with installed packages that may match, and nothing
    is guessed at or installed in its place. Extend the table before rolling this out.

    Windows PowerShell 5.1 and PowerShell 7 compatible.

    Exit codes (see remediation/README.md):
        0  every listed product is installed (or already up to date); dry run: every one is actionable
        1  bad input - the parameter string is missing, malformed, not base64/UTF-8, or empty
        2  nothing matched - every product is unmapped or not installed on this device
        3  incomplete - at least one product failed or was not offered while others were handled
        4  installed, but a reboot is required and rebootBehavior=Never suppressed it
        5  environment error - not elevated, or Microsoft.WinGet.Client could not be installed or run
        10 no install mechanism - reserved for a hook that reports NotImplemented; this one never does

.NOTES
    Dot-sourcing this file (". .\Install-SelectedSoftwarePatches.ps1") only defines the functions;
    remediation/tests/ relies on that to test the decoder and the install hook.
#>

#region toolkit-parameter-parser
# Kept byte-identical in Install-SelectedWindowsUpdates.ps1 and Install-SelectedSoftwarePatches.ps1
# (NinjaOne library scripts are single files); the Pester suite asserts the two copies match.

function New-ToolkitInputError {
    param([Parameter(Mandatory = $true)][string]$Message)
    return New-Object System.ArgumentException -ArgumentList $Message
}

function ConvertFrom-ToolkitArgumentLine {
    <#
        Parses the toolkit's space-separated "key=value" argument line into a hashtable keyed by
        the canonical spelling in AllowedKeys. Keys match case-insensitively; an unknown key, a
        repeated key or a token without "=" is an error rather than something to skip, because a
        typo such as "dryrun=false" or "dry_run=false" must not silently fall back to a default.
        An argument that arrives as an array (PowerShell's argument mode turns "a=1,2" into
        @("a=1", "2")) is re-joined with commas first.
    #>
    param(
        [AllowNull()][object[]]$ArgumentList,
        [Parameter(Mandatory = $true)][string[]]$AllowedKeys
    )
    $tokens = New-Object System.Collections.Generic.List[string]
    foreach ($arg in @($ArgumentList)) {
        if ($null -eq $arg) { continue }
        if ($arg -is [System.Array]) {
            $text = (@($arg) | ForEach-Object { [string]$_ }) -join ','
        } else {
            $text = [string]$arg
        }
        foreach ($piece in ($text -split '\s+')) {
            if ($piece.Length -gt 0) { $tokens.Add($piece) }
        }
    }
    $parsed = @{}
    foreach ($token in $tokens) {
        $eq = $token.IndexOf('=')
        if ($eq -lt 1) {
            throw (New-ToolkitInputError "Unrecognized argument '$token': expected key=value.")
        }
        $name = $token.Substring(0, $eq)
        $value = $token.Substring($eq + 1)
        $canonical = $null
        foreach ($key in $AllowedKeys) {
            if ($key -ieq $name) { $canonical = $key; break }
        }
        if ($null -eq $canonical) {
            throw (New-ToolkitInputError "Unknown parameter '$name'. Expected: $($AllowedKeys -join ', ').")
        }
        if ($parsed.ContainsKey($canonical)) {
            throw (New-ToolkitInputError "Parameter '$canonical' was given more than once.")
        }
        $parsed[$canonical] = $value
    }
    return $parsed
}

function Merge-ToolkitEnvironment {
    <#
        Fills every key the argument line did not carry from Environment (NinjaOne injects a
        declared script variable as an environment variable of the same name). The argument line
        always wins: it is what the toolkit composed for this device. An empty environment value
        counts as absent, because NinjaOne sets a declared-but-blank variable to "".
    #>
    param(
        [Parameter(Mandatory = $true)][hashtable]$Parsed,
        [AllowNull()][hashtable]$Environment,
        [Parameter(Mandatory = $true)][string[]]$AllowedKeys
    )
    if ($null -eq $Environment) { return $Parsed }
    foreach ($key in $AllowedKeys) {
        if ($Parsed.ContainsKey($key)) { continue }
        foreach ($envKey in $Environment.Keys) {
            if ($envKey -ieq $key) {
                $value = [string]$Environment[$envKey]
                if (-not [string]::IsNullOrWhiteSpace($value)) { $Parsed[$key] = $value.Trim() }
                break
            }
        }
    }
    return $Parsed
}

function Get-ToolkitEnvironment {
    param([Parameter(Mandatory = $true)][string[]]$AllowedKeys)
    $found = @{}
    foreach ($key in $AllowedKeys) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($null -ne $value) { $found[$key] = $value }
    }
    return $found
}

function ConvertTo-ToolkitBoolean {
    # The toolkit sends Rust's bool spelling ("true"/"false"); a NinjaOne checkbox variable
    # arrives the same way. 1/0 are accepted for hand-run convenience; anything else is an error.
    param([Parameter(Mandatory = $true)][string]$Name, [AllowNull()][string]$Value)
    switch -Regex (([string]$Value).Trim()) {
        '^(?i:true|1)$' { return $true }
        '^(?i:false|0)$' { return $false }
    }
    throw (New-ToolkitInputError "Parameter '$Name' must be true or false, got '$Value'.")
}

function ConvertTo-ToolkitRebootBehavior {
    # The toolkit's vocabulary is RebootChoice::script_value in src-tauri/src/actions/kind.rs.
    param([AllowNull()][string]$Value)
    switch -Regex (([string]$Value).Trim()) {
        '^(?i:never)$' { return 'Never' }
        '^(?i:auto)$' { return 'Auto' }
    }
    throw (New-ToolkitInputError "Parameter 'rebootBehavior' must be Never or Auto, got '$Value'.")
}

function Get-ToolkitExitCode {
    <#
        Folds per-target outcomes into the documented exit code. States:
          Installed, AlreadyInstalled, WouldInstall  - the target is (or would be) in place
          NotOffered                                 - the device does not know the target
          Hidden, Failed                             - the target matched but is not installed
          NotImplemented                             - no install mechanism (software script)
    #>
    param(
        [Parameter(Mandatory = $true)][string[]]$States,
        [bool]$RebootPending,
        [bool]$RebootScheduled,
        [bool]$DryRun
    )
    if (@($States | Where-Object { $_ -eq 'NotImplemented' }).Count -gt 0) { return 10 }
    if (@($States | Where-Object { $_ -ne 'NotOffered' }).Count -eq 0) { return 2 }
    $good = @('Installed', 'AlreadyInstalled', 'WouldInstall')
    if (@($States | Where-Object { $good -notcontains $_ }).Count -gt 0) { return 3 }
    if (-not $DryRun -and $RebootPending -and -not $RebootScheduled) { return 4 }
    return 0
}
#endregion toolkit-parameter-parser

function Get-SoftwareParameterKeys { return @('productAllowListB64', 'rebootBehavior', 'dryRun') }

function ConvertFrom-ProductAllowList {
    <#
        Decodes productAllowListB64 exactly as the toolkit encodes it: standard base64 with
        padding, UTF-8 inside, titles joined by "|". Anything else is an error, including an empty
        list and an empty title - a script that installs nothing and exits 0 reads as success.
        Duplicate titles are dropped; order is preserved. Titles are passed on verbatim (no
        trimming), because they are NinjaOne's own strings.
    #>
    param([AllowNull()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw (New-ToolkitInputError 'productAllowListB64 is empty: refusing to run without an explicit list of products.')
    }
    $encoded = $Value.Trim()
    if ($encoded -cnotmatch '^[A-Za-z0-9+/]+={0,2}$' -or ($encoded.Length % 4) -ne 0) {
        throw (New-ToolkitInputError 'productAllowListB64 is not standard padded base64 (A-Z a-z 0-9 + / =).')
    }
    try {
        $bytes = [Convert]::FromBase64String($encoded)
        # throwOnInvalidBytes: a stray non-UTF-8 byte must fail here, not decode to U+FFFD and
        # then quietly match nothing.
        $utf8 = New-Object System.Text.UTF8Encoding -ArgumentList $false, $true
        $decoded = $utf8.GetString($bytes)
    } catch {
        throw (New-ToolkitInputError "productAllowListB64 does not decode to UTF-8 text: $($_.Exception.Message)")
    }
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($title in $decoded.Split([char]'|')) {
        if ([string]::IsNullOrWhiteSpace($title)) {
            throw (New-ToolkitInputError "productAllowListB64 decodes to an empty product title ('$decoded').")
        }
        if (-not $list.Contains($title)) { $list.Add($title) }
    }
    return , $list.ToArray()
}

function Resolve-SoftwareRemediationRequest {
    <#
        The whole input contract in one pure function: argument line + environment in, a
        validated request out, or an ArgumentException. Defaults are the safe ones: a missing
        rebootBehavior means Never and a missing dryRun means true (report only).
    #>
    param([AllowNull()][object[]]$ArgumentList, [AllowNull()][hashtable]$Environment)
    $parsed = ConvertFrom-ToolkitArgumentLine -ArgumentList $ArgumentList -AllowedKeys (Get-SoftwareParameterKeys)
    $parsed = Merge-ToolkitEnvironment -Parsed $parsed -Environment $Environment -AllowedKeys (Get-SoftwareParameterKeys)
    if (-not $parsed.ContainsKey('productAllowListB64')) {
        throw (New-ToolkitInputError 'productAllowListB64 was not provided on the argument line or as a script variable.')
    }
    $reboot = 'Never'
    if ($parsed.ContainsKey('rebootBehavior')) { $reboot = ConvertTo-ToolkitRebootBehavior $parsed['rebootBehavior'] }
    $dryRun = $true
    if ($parsed.ContainsKey('dryRun')) { $dryRun = ConvertTo-ToolkitBoolean -Name 'dryRun' -Value $parsed['dryRun'] }
    return [pscustomobject]@{
        Products       = [string[]](ConvertFrom-ProductAllowList $parsed['productAllowListB64'])
        RebootBehavior = $reboot
        DryRun         = $dryRun
    }
}

function Test-IsElevated {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-IsSystem {
    return [Security.Principal.WindowsIdentity]::GetCurrent().IsSystem
}

#region install-hook
$script:WinGetModuleName = 'Microsoft.WinGet.Client'
# Get-WinGetPackage with no filter lists every installed package; the unmapped-title diagnostic
# reads it once per run, however many titles are unmapped.
$script:InstalledPackageListing = $null

function Get-ProductPackageMap {
    <#
        NinjaOne software-patch product title (normalized by ConvertTo-ProductKey) -> winget
        package id. Tenant-specific and deliberately exhaustive rather than fuzzy: an unlisted
        title is reported NotOffered, never guessed at. To add a row, run Get-WinGetPackage (or
        `winget list`) on an endpoint and copy the Id column verbatim; an unmapped title's log
        line names the key to add and the installed packages that may match.
    #>
    return @{
        'chromeenterprisex64'        = 'Google.Chrome'
        'chromeenterprisex86'        = 'Google.Chrome'
        'googlechrome'               = 'Google.Chrome'
        '7zip'                       = '7zip.7zip'
        '7zipx64'                    = '7zip.7zip'
        'mozillafirefox'             = 'Mozilla.Firefox'
        'notepad++'                  = 'Notepad++.Notepad++'
        'awscommandlineinterfacex64' = 'Amazon.AWSCLI'
        'awscommandlineinterface'    = 'Amazon.AWSCLI'
    }
}

function ConvertTo-ProductKey {
    # NinjaOne titles vary in punctuation and sometimes carry a version, at the end or before an
    # architecture ("Google Chrome 141.0.7390.55", "7-Zip 24.08 (x64)"); the map is keyed on
    # what survives this.
    param([Parameter(Mandatory = $true)][string]$Title)
    $text = $Title.Trim() -replace '\s+v?\d+(?:\.\d+)+(?=\s|$)', ''
    return ($text -replace '[\s\(\)\[\],._-]', '').ToLowerInvariant()
}

function Get-ProductTitleToken {
    # CamelCase compounds are split so "ChromeEnterprise" can match an installed "Google Chrome".
    # -creplace, not -replace: the default is case-insensitive, which makes [a-z][A-Z] match
    # every adjacent letter pair and shred the title.
    param([Parameter(Mandatory = $true)][string]$Title)
    $text = $Title -replace '\s+v?\d+(?:\.\d+)+(?=\s|$)', ''
    $text = $text -creplace '([a-z0-9])([A-Z])', '$1 $2'
    $noise = @('x64', 'x86', 'arm64', 'the', 'and', 'for', 'inc', 'llc')
    $tokens = New-Object System.Collections.Generic.List[string]
    foreach ($word in ($text -split '[^A-Za-z0-9]+')) {
        $token = $word.ToLowerInvariant()
        if ($token.Length -lt 3 -or $noise -contains $token) { continue }
        if (-not $tokens.Contains($token)) { $tokens.Add($token) }
    }
    return $tokens.ToArray()
}

function Resolve-ScriptHost {
    <#
        Where this run has to execute for Microsoft.WinGet.Client to work. Its cmdlets refuse to
        run as SYSTEM in Windows PowerShell ("This cmdlet is not supported in Windows PowerShell",
        microsoft/winget-cli#4820) but work there under any other account, and work as SYSTEM in
        PowerShell 7 started with -MTA. Without PowerShell 7 (or without a script file to hand it)
        the run stays put, and the first cmdlet call reports how to fix the device.
    #>
    param([AllowNull()][string]$Edition, [bool]$IsSystem, [AllowNull()][string]$PwshPath, [AllowNull()][string]$ScriptPath)
    if ($Edition -eq 'Desktop' -and $IsSystem -and $PwshPath -and $ScriptPath) { return 'Relaunch' }
    return 'InProcess'
}

function Find-Pwsh {
    $command = Get-Command pwsh.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    # SYSTEM's PATH may predate the PowerShell 7 install; ProgramW6432 is the 64-bit Program
    # Files even when the agent started a 32-bit Windows PowerShell.
    foreach ($root in @($env:ProgramW6432, $env:ProgramFiles)) {
        if (-not $root) { continue }
        $candidate = Join-Path $root 'PowerShell\7\pwsh.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    return $null
}

function Get-WinGetHostHint {
    # Appended to a failed module call, so the log says what to change rather than only what broke.
    param([AllowNull()][string]$Edition, [bool]$IsSystem)
    if ($Edition -eq 'Desktop' -and $IsSystem) {
        return ' Microsoft.WinGet.Client does not run as SYSTEM in Windows PowerShell 5.1: install PowerShell 7 on this device (the script switches to it by itself), or set Run as to a Windows credential.'
    }
    return ' If App Installer is missing or out of date on this device, run Repair-WinGetPackageManager -AllUsers -Latest there once.'
}

function Install-WinGetModule {
    # Windows PowerShell 5.1's PowerShellGet needs the NuGet provider, and its .NET defaults may
    # not offer the TLS 1.2 the gallery requires.
    if ($PSVersionTable.PSEdition -ne 'Core') {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $nuget = @(Get-PackageProvider -ListAvailable -Name NuGet -ErrorAction SilentlyContinue | Where-Object { $_.Version -ge [version]'2.8.5.201' })
        if ($nuget.Count -eq 0) {
            Install-PackageProvider -Name NuGet -MinimumVersion 2.8.5.201 -Scope AllUsers -Force -ErrorAction Stop | Out-Null
        }
    }
    Install-Module -Name $script:WinGetModuleName -Repository PSGallery -Scope AllUsers -Force -ErrorAction Stop
}

function Initialize-WinGetModule {
    <#
        Loads Microsoft.WinGet.Client, installing it for all users first when it is missing.
        Returns $null when the module is ready, or why it is not (an environment error).
    #>
    if (Get-Module -Name $script:WinGetModuleName) { return $null }
    if (-not (Get-Module -ListAvailable -Name $script:WinGetModuleName)) {
        if (-not (Test-IsElevated)) {
            return "$($script:WinGetModuleName) is not installed, and installing it for all users needs an elevated session."
        }
        try {
            Install-WinGetModule
        } catch {
            return "installing $($script:WinGetModuleName) from the PowerShell Gallery failed: $($_.Exception.Message)"
        }
        $installed = @(Get-Module -ListAvailable -Name $script:WinGetModuleName | Sort-Object Version -Descending)
        if ($installed.Count -eq 0) {
            return "$($script:WinGetModuleName) reported installed but PowerShell cannot find it."
        }
        Write-Host "SETUP            : installed $($script:WinGetModuleName) $($installed[0].Version) for all users from the PowerShell Gallery."
    }
    try {
        Import-Module -Name $script:WinGetModuleName -ErrorAction Stop
    } catch {
        return "loading $($script:WinGetModuleName) failed: $($_.Exception.Message)"
    }
    return $null
}

# Thin wrappers over the module's cmdlets: the one place the hook touches WinGet, and what the
# Pester suite mocks.
function Get-InstalledWinGetPackage {
    param([Parameter(Mandatory = $true)][string]$Id)
    return @(Get-WinGetPackage -Id $Id -MatchOption EqualsCaseInsensitive -ErrorAction Stop)
}

function Get-AllInstalledWinGetPackage {
    return @(Get-WinGetPackage -ErrorAction Stop)
}

function Invoke-WinGetPackageUpdate {
    param([Parameter(Mandatory = $true)][string]$Id)
    return Update-WinGetPackage -Id $Id -MatchOption EqualsCaseInsensitive -Mode Silent -IncludeUnknown -ErrorAction Stop
}

function Format-WinGetInstallFailure {
    # PSInstallResult.ErrorMessage() dereferences ExtendedErrorCode, which is null for some
    # statuses, so the message is assembled here instead.
    param([Parameter(Mandatory = $true)][object]$Result)
    $text = "status $($Result.Status), installer exit code $($Result.InstallerErrorCode)"
    if ($null -ne $Result.ExtendedErrorCode) {
        $text += (", extended error 0x{0:X8} ({1})" -f $Result.ExtendedErrorCode.HResult, $Result.ExtendedErrorCode.Message)
    }
    return $text
}

function Test-PendingReboot {
    foreach ($key in @(
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending',
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired')) {
        if (Test-Path -LiteralPath $key) { return $true }
    }
    return $false
}

function Get-WinGetInstalledCandidate {
    <#
        Diagnostic only - never used to choose what to install. Lists installed packages whose
        name shares a word with an unmapped NinjaOne title so the operator can paste the right Id
        into Get-ProductPackageMap instead of guessing at it.
    #>
    param([Parameter(Mandatory = $true)][string]$Title)
    $tokens = @(Get-ProductTitleToken -Title $Title)
    if ($tokens.Count -eq 0) { return @() }
    if ($null -eq $script:InstalledPackageListing) {
        try {
            $script:InstalledPackageListing = @(Get-AllInstalledWinGetPackage)
        } catch {
            $script:InstalledPackageListing = @()
        }
    }
    $matched = New-Object System.Collections.Generic.List[string]
    foreach ($package in $script:InstalledPackageListing) {
        $name = ([string]$package.Name).ToLowerInvariant()
        foreach ($token in $tokens) {
            if ($name.Contains($token)) {
                $entry = "$($package.Name) [$($package.Id)]"
                if (-not $matched.Contains($entry)) { $matched.Add($entry) }
                break
            }
        }
        if ($matched.Count -ge 5) { break }
    }
    return $matched.ToArray()
}

function Install-SelectedProduct {
    <#
        Upgrades (or, with -DryRun, only evaluates) ONE product, named by its NinjaOne patch title
        such as "ChromeEnterprise (X64)". Returns:

            @{ State = '<state>'; RebootRequired = $false; Detail = '<one line for the log>' }

        State is one of:
            Installed         - this call upgraded it
            AlreadyInstalled  - the device already has the newest version the source offers
            WouldInstall      - dry run only: an upgrade would be attempted
            NotOffered        - the title is unmapped, or the mapped package is not installed here
            Failed            - an upgrade was attempted and failed, or applicability is unknown
            NotImplemented    - unused by this implementation

        Throwing is treated as an environment error (exit 5) for the whole run, so only a failed
        package lookup throws - the module itself is broken then, not this one product.
    #>
    param([Parameter(Mandatory = $true)][string]$Title, [bool]$DryRun)

    $key = ConvertTo-ProductKey -Title $Title
    $map = Get-ProductPackageMap
    if (-not $map.ContainsKey($key)) {
        $detail = "no winget package is mapped for key '$key' -- add it to Get-ProductPackageMap in this script"
        $candidates = @(Get-WinGetInstalledCandidate -Title $Title)
        if ($candidates.Count -gt 0) { $detail += "; installed packages that may match: $($candidates -join '; ')" }
        return @{ State = 'NotOffered'; RebootRequired = $false; Detail = $detail }
    }
    $packageId = $map[$key]

    try {
        $found = @(Get-InstalledWinGetPackage -Id $packageId)
    } catch {
        throw ("looking up $packageId failed: $($_.Exception.Message)" + (Get-WinGetHostHint -Edition $PSVersionTable.PSEdition -IsSystem (Test-IsSystem)))
    }
    # The match option already asks for the exact id; filtering again keeps a looser module
    # version from handing back a different package.
    $installed = @($found | Where-Object { [string]$_.Id -ieq $packageId })
    if ($installed.Count -eq 0) {
        return @{ State = 'NotOffered'; RebootRequired = $false; Detail = "$packageId is not installed on this device" }
    }
    if ($installed.Count -gt 1) {
        $versions = @($installed | ForEach-Object { [string]$_.InstalledVersion }) -join ', '
        return @{ State = 'Failed'; RebootRequired = $false; Detail = "$packageId is installed $($installed.Count) times ($versions); upgrade it by hand" }
    }
    $package = $installed[0]
    $from = [string]$package.InstalledVersion
    # An unknown installed version reads as "no update available", which would report a
    # product as patched without anyone knowing it is.
    if ([string]::IsNullOrWhiteSpace($from) -or $from -ieq 'Unknown') {
        return @{ State = 'Failed'; RebootRequired = $false; Detail = "WinGet cannot read the installed version of $packageId, so it cannot tell whether the patch applies" }
    }
    if (-not $package.IsUpdateAvailable) {
        return @{ State = 'AlreadyInstalled'; RebootRequired = $false; Detail = "$packageId $from -- no newer version offered" }
    }
    $to = @($package.AvailableVersions | Where-Object { $_ })
    $target = 'a newer version'
    if ($to.Count -gt 0) { $target = [string]$to[0] }
    if ($DryRun) {
        return @{ State = 'WouldInstall'; RebootRequired = $false; Detail = "$packageId $from -> $target" }
    }

    $rebootBefore = Test-PendingReboot
    try {
        $result = Invoke-WinGetPackageUpdate -Id $packageId
    } catch {
        return @{ State = 'Failed'; RebootRequired = $false; Detail = "upgrading $packageId failed: $($_.Exception.Message)" }
    }
    if ($null -eq $result) {
        return @{ State = 'Failed'; RebootRequired = $false; Detail = "Update-WinGetPackage returned no result for $packageId" }
    }
    if ([string]$result.Status -ne 'Ok') {
        return @{ State = 'Failed'; RebootRequired = $false; Detail = "upgrading $packageId failed: $(Format-WinGetInstallFailure -Result $result)" }
    }
    return @{
        State          = 'Installed'
        RebootRequired = ([bool]$result.RebootRequired -or ((Test-PendingReboot) -and -not $rebootBefore))
        Detail         = "$packageId $from -> $target"
    }
}
#endregion install-hook

function Invoke-SoftwareRemediation {
    param([AllowNull()][object[]]$ScriptArguments)

    try {
        $request = Resolve-SoftwareRemediationRequest -ArgumentList $ScriptArguments -Environment (Get-ToolkitEnvironment -AllowedKeys (Get-SoftwareParameterKeys))
    } catch [System.ArgumentException] {
        Write-Host "ERROR (bad input): $($_.Exception.Message)"
        Write-Host 'Expected: productAllowListB64=<base64 of titles joined by |> rebootBehavior=<Never|Auto> dryRun=<true|false>'
        return 1
    }

    $mode = 'INSTALL'
    if ($request.DryRun) { $mode = 'DRY RUN - nothing will be installed' }
    Write-Host "Mode: $mode"
    Write-Host "Products ($($request.Products.Count)):"
    foreach ($title in $request.Products) { Write-Host "  - $title" }
    Write-Host "rebootBehavior: $($request.RebootBehavior)"

    if (-not $request.DryRun -and -not (Test-IsElevated)) {
        Write-Host 'ERROR (environment): installing software needs an elevated session. Run as SYSTEM or an administrator credential.'
        return 5
    }

    $notReady = Initialize-WinGetModule
    if ($notReady) {
        Write-Host "ERROR (environment): $notReady"
        return 5
    }

    $known = @('Installed', 'AlreadyInstalled', 'WouldInstall', 'NotOffered', 'Failed', 'NotImplemented')
    $states = @()
    $rebootPending = $false
    foreach ($title in $request.Products) {
        try {
            $outcome = Install-SelectedProduct -Title $title -DryRun $request.DryRun
        } catch {
            Write-Host "ERROR (environment): install hook threw for '$title': $($_.Exception.Message)"
            return 5
        }
        $state = [string]$outcome.State
        if ($known -notcontains $state) {
            Write-Host "FAILED           '$title' : install hook returned unknown state '$state'."
            $state = 'Failed'
        } elseif ($request.DryRun -and $state -eq 'Installed') {
            # A hook that installs during a dry run is a bug worth surfacing, not a success.
            Write-Host "FAILED           '$title' : install hook reported Installed during a dry run."
            $state = 'Failed'
        } else {
            Write-Host ("{0,-16} '{1}' : {2}" -f $state.ToUpperInvariant(), $title, $outcome.Detail)
        }
        if ($outcome.RebootRequired -and -not $request.DryRun) { $rebootPending = $true }
        $states += $state
    }

    $rebootScheduled = $false
    if ($rebootPending) {
        if ($request.RebootBehavior -eq 'Auto') {
            # A delayed restart lets this script exit and the NinjaOne agent report the result
            # before the machine goes down.
            & shutdown.exe /r /t 60 /d p:4:1 /c 'NinjaOne Patch Toolkit: restarting to finish installing software updates.' | Out-Host
            if ($LASTEXITCODE -eq 0) {
                $rebootScheduled = $true
                Write-Host 'REBOOT           : restart scheduled in 60 seconds (rebootBehavior=Auto).'
            } else {
                Write-Host "REBOOT           : a restart is required but shutdown.exe failed (exit $LASTEXITCODE)."
            }
        } else {
            Write-Host 'REBOOT           : a restart is required to finish; not restarting because rebootBehavior=Never.'
        }
    }

    $exitCode = Get-ToolkitExitCode -States $states -RebootPending $rebootPending -RebootScheduled $rebootScheduled -DryRun $request.DryRun
    if ($exitCode -eq 10) {
        Write-Host 'Nothing was installed: the install hook has no install mechanism. See remediation/README.md.'
    }
    Write-Host "Result: exit $exitCode"
    return $exitCode
}

if ($MyInvocation.InvocationName -ne '.') {
    $pwsh = $null
    $isSystem = Test-IsSystem
    if ($PSVersionTable.PSEdition -eq 'Desktop' -and $isSystem) { $pwsh = Find-Pwsh }
    if ((Resolve-ScriptHost -Edition $PSVersionTable.PSEdition -IsSystem $isSystem -PwshPath $pwsh -ScriptPath $PSCommandPath) -eq 'Relaunch') {
        Write-Host "Host: re-running in PowerShell 7 ($pwsh), because Microsoft.WinGet.Client does not run as SYSTEM in Windows PowerShell."
        # The arguments are key=value tokens with no spaces or quotes, so they pass through
        # verbatim; NinjaOne's script variables reach the child through the inherited environment.
        & $pwsh -NoLogo -NoProfile -NonInteractive -MTA -ExecutionPolicy Bypass -File $PSCommandPath @args
        $code = $LASTEXITCODE
        if ($null -eq $code) {
            Write-Host 'ERROR (environment): PowerShell 7 did not start.'
            $code = 5
        }
        exit ([int]$code)
    }
    # Select the last object so a stray pipeline write can never turn the exit code into an array.
    $code = Invoke-SoftwareRemediation -ScriptArguments $args | Select-Object -Last 1
    exit ([int]$code)
}
