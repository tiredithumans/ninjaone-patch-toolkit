#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.0.0' }
# Pester 5 suite for the input contract and the WinGet install hook of
# Install-SelectedSoftwarePatches.ps1. Dot-sourcing the script only defines its functions; the
# hook's Microsoft.WinGet.Client calls go through thin wrappers that every test here mocks, so
# nothing touches WinGet and the suite runs on Linux and macOS too. Run with
#   Invoke-Pester -Path remediation/tests

BeforeDiscovery {
    $fixture = Get-Content -Raw -Encoding UTF8 -Path (Join-Path $PSScriptRoot 'fixtures/parameter-contract.json') | ConvertFrom-Json
    $contractCases = @($fixture.cases | Where-Object { $_.kind -eq 'SOFTWARE_PATCH_REMEDIATE' } | ForEach-Object {
            @{ Name = $_.name; Parameters = $_.parameters; Expect = $_.expect }
        })
}

BeforeAll {
    $remediationDir = Split-Path -Parent $PSScriptRoot
    $scriptPath = Join-Path $remediationDir 'Install-SelectedSoftwarePatches.ps1'
    . $scriptPath

    function Split-AsNinjaOne([string]$Line) { return , @($Line -split ' ') }
    function ConvertTo-Base64Utf8([string]$Text) { return [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($Text)) }
    function Get-ParserRegion([string]$Path) {
        $text = [System.IO.File]::ReadAllText($Path)
        $match = [regex]::Match($text, '(?s)#region toolkit-parameter-parser.*?#endregion toolkit-parameter-parser')
        if (-not $match.Success) { throw "no parser region in $Path" }
        return $match.Value -replace "`r`n", "`n"
    }

    # Compares two lists as one string: Should -Be on a one-element array unrolls the actual
    # value, so element-by-element comparison is not reliable across Pester 5 versions.
    function Join-List([object[]]$Items) { return (@($Items) | ForEach-Object { [string]$_ }) -join [char]0x1f }
}

Describe 'Install-SelectedSoftwarePatches.ps1 source' {
    It 'parses without errors' {
        $tokens = $null; $errors = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
    }

    It 'is pure ASCII, so Windows PowerShell 5.1 reads it the same without a BOM' {
        $bytes = [System.IO.File]::ReadAllBytes($scriptPath)
        @($bytes | Where-Object { $_ -gt 127 }).Count | Should -Be 0
    }

    It 'carries the same parameter parser as Install-SelectedWindowsUpdates.ps1' {
        Get-ParserRegion $scriptPath |
            Should -BeExactly (Get-ParserRegion (Join-Path $remediationDir 'Install-SelectedWindowsUpdates.ps1'))
    }

    # A missing space once fused the reason code and the comment switch (`p:4:1/c`), which
    # shutdown.exe rejects, so rebootBehavior=Auto never restarted anything. Nothing here runs
    # shutdown.exe, so the arguments are checked as the parser sees them.
    It 'passes shutdown.exe its reason code and comment as separate switches' {
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$null, [ref]$null)
        $calls = @($ast.FindAll({
                    param($n)
                    $n -is [System.Management.Automation.Language.CommandAst] -and $n.GetCommandName() -eq 'shutdown.exe'
                }, $true))
        $calls.Count | Should -BeGreaterThan 0
        foreach ($call in $calls) {
            $words = @($call.CommandElements | ForEach-Object { $_.Extent.Text })
            $words -contains '/c' | Should -BeTrue
            $words[$words.IndexOf('/d') + 1] | Should -Match '^p:\d+:\d+$'
        }
    }
}

Describe 'the toolkit parameter contract (fixture shared with build_parameters)' {
    It '<Name>' -ForEach $contractCases {
        $argumentList = Split-AsNinjaOne $Parameters
        if ($Expect.error) {
            { Resolve-SoftwareRemediationRequest -ArgumentList $argumentList -Environment @{} } |
                Should -Throw -ExceptionType ([System.ArgumentException])
        } else {
            $request = Resolve-SoftwareRemediationRequest -ArgumentList $argumentList -Environment @{}
            Join-List ($request.Products) | Should -BeExactly (Join-List @($Expect.products))
            $request.RebootBehavior | Should -BeExactly $Expect.rebootBehavior
            $request.DryRun | Should -Be $Expect.dryRun
        }
    }
}

Describe 'ConvertFrom-ProductAllowList' {
    It 'keeps titles verbatim, including surrounding spaces' {
        Join-List (ConvertFrom-ProductAllowList (ConvertTo-Base64Utf8 ' Tool A |Tool B')) | Should -BeExactly (Join-List @(' Tool A ', 'Tool B'))
    }

    It 'drops a repeated title and keeps the first-seen order' {
        Join-List (ConvertFrom-ProductAllowList (ConvertTo-Base64Utf8 'B|A|B')) | Should -BeExactly (Join-List @('B', 'A'))
    }

    It 'rejects <Why>' -ForEach @(
        @{ Why = 'an empty value'; Value = '' }
        @{ Why = 'characters outside the base64 alphabet'; Value = 'not-base64!' }
        @{ Why = 'the URL-safe alphabet'; Value = 'UGFpbnQuTkVUIDUuMC4xMyB-IGJldGE_' }
        @{ Why = 'missing padding'; Value = 'UGFpbnQ' }
        @{ Why = 'bytes that are not UTF-8'; Value = '/w==' }
        @{ Why = 'an empty title between separators'; Value = 'YXx8Yg==' }
        @{ Why = 'a whitespace-only title'; Value = 'YXwgfGI=' }
    ) {
        { ConvertFrom-ProductAllowList $Value } | Should -Throw -ExceptionType ([System.ArgumentException])
    }
}

Describe 'Resolve-SoftwareRemediationRequest' {
    It 'reads script variables from the environment when the argument line is empty' {
        $request = Resolve-SoftwareRemediationRequest -ArgumentList @() -Environment @{
            productAllowListB64 = (ConvertTo-Base64Utf8 '7-Zip'); dryRun = 'false'
        }
        Join-List ($request.Products) | Should -BeExactly (Join-List @('7-Zip'))
        $request.DryRun | Should -BeFalse
        $request.RebootBehavior | Should -BeExactly 'Never'
    }

    It 'rejects a KB list sent to the software script' {
        { Resolve-SoftwareRemediationRequest -ArgumentList @('kbAllowList=5040434') -Environment @{} } |
            Should -Throw -ExceptionType ([System.ArgumentException])
    }
}

Describe 'Invoke-SoftwareRemediation' {
    BeforeAll {
        $oneProduct = "productAllowListB64=$(ConvertTo-Base64Utf8 'Google Chrome 141.0.7390.55')"
    }

    BeforeEach {
        Mock Initialize-WinGetModule { $null }
    }

    It 'exits 2 when the only product is unmapped, even on a dry run' {
        Mock Get-AllInstalledWinGetPackage { @() }
        $script:InstalledPackageListing = $null
        $unmapped = "productAllowListB64=$(ConvertTo-Base64Utf8 'Some Unlisted Tool 1.0')"
        Invoke-SoftwareRemediation -ScriptArguments @($unmapped, 'dryRun=true') 6>$null | Should -Be 2
    }

    It 'exits 5 without calling the hook when Microsoft.WinGet.Client is not ready' {
        Mock Initialize-WinGetModule { 'module missing' }
        Mock Install-SelectedProduct { throw 'must not be called' }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 5
        Should -Invoke Install-SelectedProduct -Times 0
    }

    It 'exits 5 when the hook throws' {
        Mock Install-SelectedProduct { throw 'module broken' }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 5
    }

    It 'exits 1 on bad input without calling the hook' {
        Mock Install-SelectedProduct { throw 'must not be called' }
        Invoke-SoftwareRemediation -ScriptArguments @('productAllowListB64=', 'dryRun=true') 6>$null | Should -Be 1
        Should -Invoke Install-SelectedProduct -Times 0
    }

    It 'exits 5 when a real install is not elevated' {
        Mock Test-IsElevated { $false }
        Mock Install-SelectedProduct { throw 'must not be called' }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=false') 6>$null | Should -Be 5
        Should -Invoke Install-SelectedProduct -Times 0
    }

    It 'exits 4 when an install needs a reboot and rebootBehavior=Never' {
        Mock Test-IsElevated { $true }
        Mock Install-SelectedProduct { @{ State = 'Installed'; RebootRequired = $true; Detail = 'ok' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=false', 'rebootBehavior=Never') 6>$null | Should -Be 4
    }

    It 'exits 3 when a hook claims Installed during a dry run' {
        Mock Install-SelectedProduct { @{ State = 'Installed'; RebootRequired = $false; Detail = 'oops' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 3
    }

    It 'exits 3 when a hook returns an unknown state' {
        Mock Install-SelectedProduct { @{ State = 'Done'; RebootRequired = $false; Detail = '' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 3
    }

    It 'exits 0 when a dry run finds every product actionable' {
        Mock Install-SelectedProduct { @{ State = 'WouldInstall'; RebootRequired = $false; Detail = 'would upgrade' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 0
    }
}

Describe 'ConvertTo-ProductKey' {
    It 'maps <Title> to <Key>' -ForEach @(
        @{ Title = 'ChromeEnterprise (X64)'; Key = 'chromeenterprisex64' }
        @{ Title = 'Google Chrome 141.0.7390.55'; Key = 'googlechrome' }
        @{ Title = '7-Zip 24.08 (x64)'; Key = '7zipx64' }
        @{ Title = 'Notepad++ 8.6.9'; Key = 'notepad++' }
        @{ Title = 'AWS Command Line Interface v2.17.0'; Key = 'awscommandlineinterface' }
        @{ Title = 'Edge (X64)'; Key = 'edgex64' }
        @{ Title = 'Edge (X86)'; Key = 'edgex86' }
        @{ Title = 'Microsoft Edge 137.0.3296.62'; Key = 'microsoftedge' }
    ) {
        ConvertTo-ProductKey -Title $Title | Should -BeExactly $Key
    }

    It 'maps every Edge title to the one Microsoft.Edge package' {
        $map = Get-ProductPackageMap
        foreach ($title in @('Edge (X64)', 'Edge (X86)', 'Microsoft Edge 137.0.3296.62')) {
            $map[(ConvertTo-ProductKey -Title $title)] | Should -BeExactly 'Microsoft.Edge'
        }
    }

    It 'gives every mapped key a non-empty package id' {
        $map = Get-ProductPackageMap
        foreach ($key in $map.Keys) {
            ConvertTo-ProductKey -Title $key | Should -BeExactly $key
            $map[$key] | Should -Not -BeNullOrEmpty
        }
    }
}

Describe 'Resolve-ScriptHost' {
    It 'relaunches in PowerShell 7 only as SYSTEM in Windows PowerShell with pwsh and a script file' {
        Resolve-ScriptHost -Edition 'Desktop' -IsSystem $true -PwshPath 'C:\pwsh.exe' -ScriptPath 'C:\s.ps1' | Should -BeExactly 'Relaunch'
    }

    It 'stays in process for <Why>' -ForEach @(
        @{ Why = 'a credential in Windows PowerShell'; Edition = 'Desktop'; IsSystem = $false; Pwsh = 'C:\pwsh.exe'; Path = 'C:\s.ps1' }
        @{ Why = 'SYSTEM already in PowerShell 7'; Edition = 'Core'; IsSystem = $true; Pwsh = 'C:\pwsh.exe'; Path = 'C:\s.ps1' }
        @{ Why = 'SYSTEM without PowerShell 7'; Edition = 'Desktop'; IsSystem = $true; Pwsh = $null; Path = 'C:\s.ps1' }
        @{ Why = 'SYSTEM without a script file'; Edition = 'Desktop'; IsSystem = $true; Pwsh = 'C:\pwsh.exe'; Path = '' }
    ) {
        Resolve-ScriptHost -Edition $Edition -IsSystem $IsSystem -PwshPath $Pwsh -ScriptPath $Path | Should -BeExactly 'InProcess'
    }

    It 'tells SYSTEM in Windows PowerShell how to fix the device' {
        Get-WinGetHostHint -Edition 'Desktop' -IsSystem $true | Should -Match 'PowerShell 7'
        Get-WinGetHostHint -Edition 'Core' -IsSystem $true | Should -Match 'Repair-WinGetPackageManager'
    }

    It 'tells a credential run in <Edition> how to register App Installer for its account' -ForEach @(
        @{ Edition = 'Desktop' }
        @{ Edition = 'Core' }
    ) {
        $hint = Get-WinGetHostHint -Edition $Edition -IsSystem $false
        $hint | Should -Match '0x800706BA'
        $hint | Should -Match 'Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft\.DesktopAppInstaller_8wekyb3d8bbwe'
    }

    It 'gives SYSTEM in PowerShell 7 no per-account registration advice' {
        $hint = Get-WinGetHostHint -Edition 'Core' -IsSystem $true
        $hint | Should -Not -Match '0x800706BA'
        $hint | Should -Not -Match 'Add-AppxPackage'
    }
}

Describe 'Install-SelectedProduct' {
    BeforeAll {
        function New-FakePackage([string]$Version, [bool]$Update, [string[]]$Available = @('156.0.8078.48')) {
            return [pscustomobject]@{
                Id = 'Google.Chrome'; Name = 'Google Chrome'; InstalledVersion = $Version
                IsUpdateAvailable = $Update; AvailableVersions = $Available
            }
        }
        $chrome = 'ChromeEnterprise (X64)'
    }

    BeforeEach {
        Mock Test-IsSystem { $true }
        Mock Test-PendingReboot { $false }
        Mock Invoke-WinGetPackageUpdate { throw 'must not be called' }
        $script:InstalledPackageListing = $null
    }

    It 'reports an unmapped title as NotOffered and names installed packages that may match' {
        Mock Get-InstalledWinGetPackage { throw 'must not be called' }
        Mock Get-AllInstalledWinGetPackage { @([pscustomobject]@{ Id = 'Vendor.Widget'; Name = 'Widget Studio' }) }
        $outcome = Install-SelectedProduct -Title 'Widget Studio Pro 3.1' -DryRun $true
        $outcome.State | Should -BeExactly 'NotOffered'
        $outcome.Detail | Should -Match "'widgetstudiopro'"
        $outcome.Detail | Should -Match 'Widget Studio \[Vendor\.Widget\]'
    }

    It 'reports a mapped package that is not installed as NotOffered' {
        Mock Get-InstalledWinGetPackage { @() }
        (Install-SelectedProduct -Title $chrome -DryRun $true).State | Should -BeExactly 'NotOffered'
    }

    It 'looks an Edge title up under Microsoft.Edge' {
        Mock Get-InstalledWinGetPackage -ParameterFilter { $Id -eq 'Microsoft.Edge' } {
            @([pscustomobject]@{
                    Id = 'Microsoft.Edge'; Name = 'Microsoft Edge'; InstalledVersion = '137.0.3296.62'
                    IsUpdateAvailable = $true; AvailableVersions = @('138.0.3351.55')
                })
        }
        Mock Get-InstalledWinGetPackage { throw 'looked up the wrong package id' }
        (Install-SelectedProduct -Title 'Edge (X64)' -DryRun $true).State | Should -BeExactly 'WouldInstall'
    }

    It 'ignores a package whose id is not the mapped one' {
        Mock Get-InstalledWinGetPackage { @([pscustomobject]@{ Id = 'Google.Chrome.Beta'; InstalledVersion = '1.0'; IsUpdateAvailable = $true }) }
        (Install-SelectedProduct -Title $chrome -DryRun $true).State | Should -BeExactly 'NotOffered'
    }

    It 'reports AlreadyInstalled when no update is available' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage '156.0.8078.48' $false) }
        (Install-SelectedProduct -Title $chrome -DryRun $false).State | Should -BeExactly 'AlreadyInstalled'
    }

    It 'fails rather than reporting success when the installed version is unknown' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage 'Unknown' $false) }
        (Install-SelectedProduct -Title $chrome -DryRun $true).State | Should -BeExactly 'Failed'
    }

    It 'fails when the package is installed more than once' {
        Mock Get-InstalledWinGetPackage { @((New-FakePackage '155.0' $true), (New-FakePackage '154.0' $true)) }
        (Install-SelectedProduct -Title $chrome -DryRun $true).State | Should -BeExactly 'Failed'
    }

    It 'only evaluates on a dry run' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage '155.0.8059.85' $true) }
        $outcome = Install-SelectedProduct -Title $chrome -DryRun $true
        $outcome.State | Should -BeExactly 'WouldInstall'
        $outcome.Detail | Should -BeExactly 'Google.Chrome 155.0.8059.85 -> 156.0.8078.48'
        Should -Invoke Invoke-WinGetPackageUpdate -Times 0
    }

    It 'upgrades and passes the module''s reboot flag through' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage '155.0.8059.85' $true) }
        Mock Invoke-WinGetPackageUpdate { [pscustomobject]@{ Status = 'Ok'; RebootRequired = $true; InstallerErrorCode = 0; ExtendedErrorCode = $null } }
        $outcome = Install-SelectedProduct -Title $chrome -DryRun $false
        $outcome.State | Should -BeExactly 'Installed'
        $outcome.RebootRequired | Should -BeTrue
        Should -Invoke Invoke-WinGetPackageUpdate -Times 1 -ParameterFilter { $Id -eq 'Google.Chrome' }
    }

    It 'reports a failed upgrade with its status and installer exit code' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage '155.0.8059.85' $true) }
        Mock Invoke-WinGetPackageUpdate { [pscustomobject]@{ Status = 'InstallError'; RebootRequired = $false; InstallerErrorCode = 1603; ExtendedErrorCode = $null } }
        $outcome = Install-SelectedProduct -Title $chrome -DryRun $false
        $outcome.State | Should -BeExactly 'Failed'
        $outcome.Detail | Should -Match 'InstallError.*1603'
    }

    It 'reports an upgrade that throws as Failed for that product only' {
        Mock Get-InstalledWinGetPackage { @(New-FakePackage '155.0.8059.85' $true) }
        Mock Invoke-WinGetPackageUpdate { throw 'installer download failed' }
        (Install-SelectedProduct -Title $chrome -DryRun $false).State | Should -BeExactly 'Failed'
    }

    It 'throws, with a hint, when the package lookup itself fails' {
        Mock Get-InstalledWinGetPackage { throw 'This cmdlet is not supported in Windows PowerShell.' }
        { Install-SelectedProduct -Title $chrome -DryRun $true } | Should -Throw '*not supported in Windows PowerShell*'
    }
}
