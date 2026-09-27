#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.0.0' }
# Pester 5 suite for the input contract of Install-SelectedWindowsUpdates.ps1. It never touches the
# Windows Update Agent: dot-sourcing the script only defines its functions. Run with
#   Invoke-Pester -Path remediation/tests

BeforeDiscovery {
    $fixture = Get-Content -Raw -Encoding UTF8 -Path (Join-Path $PSScriptRoot 'fixtures/parameter-contract.json') | ConvertFrom-Json
    $contractCases = @($fixture.cases | Where-Object { $_.kind -eq 'OS_PATCH_REMEDIATE' } | ForEach-Object {
            @{ Name = $_.name; Parameters = $_.parameters; Expect = $_.expect }
        })
}

BeforeAll {
    $scriptPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'Install-SelectedWindowsUpdates.ps1'
    . $scriptPath

    # NinjaOne splits the toolkit's parameter string on spaces and passes the pieces as arguments.
    function Split-AsNinjaOne([string]$Line) { return , @($Line -split ' ') }

    # Compares two lists as one string: Should -Be on a one-element array unrolls the actual
    # value, so element-by-element comparison is not reliable across Pester 5 versions.
    function Join-List([object[]]$Items) { return (@($Items) | ForEach-Object { [string]$_ }) -join [char]0x1f }
}

Describe 'Install-SelectedWindowsUpdates.ps1 source' {
    It 'parses without errors' {
        $tokens = $null; $errors = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
    }

    It 'is pure ASCII, so Windows PowerShell 5.1 reads it the same without a BOM' {
        $bytes = [System.IO.File]::ReadAllBytes($scriptPath)
        @($bytes | Where-Object { $_ -gt 127 }).Count | Should -Be 0
    }
}

Describe 'the toolkit parameter contract (fixture shared with build_parameters)' {
    It '<Name>' -ForEach $contractCases {
        $argumentList = Split-AsNinjaOne $Parameters
        if ($Expect.error) {
            { Resolve-OsRemediationRequest -ArgumentList $argumentList -Environment @{} } |
                Should -Throw -ExceptionType ([System.ArgumentException])
        } else {
            $request = Resolve-OsRemediationRequest -ArgumentList $argumentList -Environment @{}
            Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @($Expect.kbNumbers))
            $request.RebootBehavior | Should -BeExactly $Expect.rebootBehavior
            $request.DryRun | Should -Be $Expect.dryRun
        }
    }

    It '<Name> (whole line as one argument)' -ForEach @($contractCases | Where-Object { -not $_.Expect.error }) {
        $request = Resolve-OsRemediationRequest -ArgumentList @($Parameters) -Environment @{}
        Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @($Expect.kbNumbers))
    }
}

Describe 'Resolve-OsRemediationRequest' {
    It 're-joins an argument PowerShell split on commas' {
        $request = Resolve-OsRemediationRequest -ArgumentList @(, @('kbAllowList=5040434', '5041580')) -Environment @{}
        Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @('5040434', '5041580'))
    }

    It 'defaults to rebootBehavior=Never and dryRun=true' {
        $request = Resolve-OsRemediationRequest -ArgumentList @('kbAllowList=5040434') -Environment @{}
        $request.RebootBehavior | Should -BeExactly 'Never'
        $request.DryRun | Should -BeTrue
    }

    It 'accepts keys and values case-insensitively' {
        $request = Resolve-OsRemediationRequest -ArgumentList @('KBALLOWLIST=kb5040434', 'rebootbehavior=AUTO', 'DryRun=FALSE') -Environment @{}
        Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @('5040434'))
        $request.RebootBehavior | Should -BeExactly 'Auto'
        $request.DryRun | Should -BeFalse
    }

    It 'drops a repeated KB' {
        Join-List (Resolve-OsRemediationRequest -ArgumentList @('kbAllowList=5040434,KB5040434,5041580') -Environment @{}).KbNumbers |
            Should -BeExactly (Join-List @('5040434', '5041580'))
    }

    It 'reads script variables from the environment when the argument line is empty' {
        $request = Resolve-OsRemediationRequest -ArgumentList @() -Environment @{
            kbAllowList = '5040434'; rebootBehavior = 'Auto'; dryRun = 'false'
        }
        Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @('5040434'))
        $request.RebootBehavior | Should -BeExactly 'Auto'
        $request.DryRun | Should -BeFalse
    }

    It 'lets the argument line win over the environment, key by key' {
        $request = Resolve-OsRemediationRequest -ArgumentList @('kbAllowList=1', 'dryRun=true') -Environment @{
            kbAllowList = '2'; dryRun = 'false'; rebootBehavior = 'Auto'
        }
        Join-List ($request.KbNumbers) | Should -BeExactly (Join-List @('1'))
        $request.DryRun | Should -BeTrue
        $request.RebootBehavior | Should -BeExactly 'Auto'
    }

    It 'treats a blank environment variable as absent' {
        { Resolve-OsRemediationRequest -ArgumentList @() -Environment @{ kbAllowList = '  ' } } |
            Should -Throw -ExceptionType ([System.ArgumentException])
    }

    It 'rejects <Why>' -ForEach @(
        @{ Why = 'a missing allow list'; Line = 'rebootBehavior=Never dryRun=true' }
        @{ Why = 'a non-digit KB'; Line = 'kbAllowList=5040434,12a' }
        @{ Why = 'an empty entry'; Line = 'kbAllowList=5040434,,5041580' }
        @{ Why = 'a trailing comma'; Line = 'kbAllowList=5040434,' }
        @{ Why = 'a negative number'; Line = 'kbAllowList=-5040434' }
        @{ Why = 'an unknown key'; Line = 'kbAllowList=5040434 dry_run=false' }
        @{ Why = 'a repeated key'; Line = 'kbAllowList=5040434 dryRun=true dryRun=false' }
        @{ Why = 'a token without ='; Line = 'kbAllowList=5040434 -Force' }
        @{ Why = 'a bad dryRun value'; Line = 'kbAllowList=5040434 dryRun=yes' }
        @{ Why = 'a bad rebootBehavior value'; Line = 'kbAllowList=5040434 rebootBehavior=Always' }
    ) {
        { Resolve-OsRemediationRequest -ArgumentList (Split-AsNinjaOne $Line) -Environment @{} } |
            Should -Throw -ExceptionType ([System.ArgumentException])
    }

    It 'rejects non-ASCII digits' {
        $arabicIndic = [string][char]0x0661 + [char]0x0662
        { Resolve-OsRemediationRequest -ArgumentList @("kbAllowList=$arabicIndic") -Environment @{} } |
            Should -Throw -ExceptionType ([System.ArgumentException])
    }
}

Describe 'Get-ToolkitExitCode' {
    It '<Why> -> <Code>' -ForEach @(
        @{ Why = 'all installed'; States = @('Installed', 'AlreadyInstalled'); Reboot = $false; Scheduled = $false; Dry = $false; Code = 0 }
        @{ Why = 'dry run, all actionable'; States = @('WouldInstall', 'AlreadyInstalled'); Reboot = $false; Scheduled = $false; Dry = $true; Code = 0 }
        @{ Why = 'nothing offered'; States = @('NotOffered', 'NotOffered'); Reboot = $false; Scheduled = $false; Dry = $false; Code = 2 }
        @{ Why = 'one not offered'; States = @('Installed', 'NotOffered'); Reboot = $false; Scheduled = $false; Dry = $false; Code = 3 }
        @{ Why = 'one failed'; States = @('Installed', 'Failed'); Reboot = $true; Scheduled = $false; Dry = $false; Code = 3 }
        @{ Why = 'hidden only'; States = @('Hidden'); Reboot = $false; Scheduled = $false; Dry = $true; Code = 3 }
        @{ Why = 'reboot suppressed'; States = @('Installed'); Reboot = $true; Scheduled = $false; Dry = $false; Code = 4 }
        @{ Why = 'reboot scheduled'; States = @('Installed'); Reboot = $true; Scheduled = $true; Dry = $false; Code = 0 }
        @{ Why = 'pending reboot on a dry run'; States = @('WouldInstall'); Reboot = $true; Scheduled = $false; Dry = $true; Code = 0 }
    ) {
        Get-ToolkitExitCode -States $States -RebootPending $Reboot -RebootScheduled $Scheduled -DryRun $Dry |
            Should -Be $Code
    }
}

Describe 'Invoke-OsRemediation' {
    It 'exits 1 on bad input before touching Windows Update' {
        Mock Search-WindowsUpdate { throw 'must not be called' }
        Invoke-OsRemediation -ScriptArguments @('kbAllowList=') 6>$null | Should -Be 1
        Should -Invoke Search-WindowsUpdate -Times 0
    }
}
