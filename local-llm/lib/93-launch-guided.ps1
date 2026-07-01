# Guided, plain-language launcher (Spectre) for non-developers.
#
# Surfaces friendly words — Run with / Quality / Memory / Speed / Images — over the
# technical launch fields, offers a "Launch now (recommended)" vs "Customize" fork,
# and explains each choice inline. The plain-language vocabulary + summary below are
# pure (unit-tested); the interactive flow composes them with PwshSpectreConsole and
# reuses Resolve-LaunchPlan / Build-LaunchSelectionArgs / Invoke-LLMSelection.

function Get-GuidedTargetLabel {
    # Friendly name for a run target (action).
    param([Parameter(Mandatory = $true)][string]$Value)
    switch ($Value) {
        'localpilot' { 'LocalPilot (recommended)' }
        'claude' { 'Claude Code' }
        'codex' { 'Codex' }
        'serve' { 'Share to other apps' }
        default { $Value }
    }
}

function Get-GuidedEngineLabel {
    # Friendly name for the llama.cpp mode.
    param([Parameter(Mandatory = $true)][string]$Value)
    switch ($Value) {
        'native' { 'Standard' }
        'turboquant' { 'Turbo (auto-tuned for your GPU)' }
        'mtpturbo' { 'Turbo+ (draft speed-ups)' }
        default { $Value }
    }
}

function Get-GuidedMemoryLabel {
    # Friendly name for a context window.
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [AllowEmptyString()][string]$ContextKey
    )
    $tokens = ''
    if ($Def.Contains('Contexts') -and $Def['Contexts'].Contains($ContextKey)) {
        $tokens = ' (~{0:N0} words)' -f ([int]$Def['Contexts'][$ContextKey] * 0.75)
    }
    if ([string]::IsNullOrEmpty($ContextKey)) { return "Standard$tokens" }
    return "Large — $ContextKey$tokens"
}

function Get-GuidedQualityLabel {
    # Friendly name for a quant: size + a plain quality hint.
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][string]$Quant
    )
    $size = ''
    if ($Def.Contains('QuantSizesGB') -and $Def['QuantSizesGB'].Contains($Quant)) {
        $size = ' · {0:N1} GB' -f [double]$Def['QuantSizesGB'][$Quant]
    }
    $hint = if ($Quant -match 'compact|mini|Q4') { 'smaller & faster' }
    elseif ($Quant -match 'quality|Q6|Q8') { 'best quality' }
    else { 'balanced' }
    return "$hint$size"
}

function Format-GuidedPlanSummary {
    # The recommended plan in plain words. Pure.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][pscustomobject]$Plan,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )
    $name = if ($Def.Contains('DisplayName')) { [string]$Def['DisplayName'] } else { $Plan.ModelKey }
    $speed = if ($Plan.UseAutoBest) { '{0} · auto-tuned' -f (Get-GuidedEngineLabel -Value $Plan.Mode) } else { Get-GuidedEngineLabel -Value $Plan.Mode }
    @(
        ('Model:     {0}' -f $name)
        ('Run with:  {0}' -f (Get-GuidedTargetLabel -Value $Plan.Target))
        ('Quality:   {0}' -f (Get-GuidedQualityLabel -Def $Def -Quant $Plan.Quant))
        ('Memory:    {0}' -f (Get-GuidedMemoryLabel -Def $Def -ContextKey $Plan.ContextKey))
        ('Speed:     {0}' -f $speed)
        ('Images:    {0}' -f $(if ($Plan.Vision) { 'on' } else { 'off' }))
    ) -join "`n"
}

function Get-GuidedGlossary {
    # Plain-language help: what each choice means and when to change it.
    @'
What these mean:

  Run with  – which coding assistant drives the model. LocalPilot is the
              built-in one and the safe default.
  Quality   – bigger files understand more but use more graphics memory (GB);
              smaller ones are faster and lighter. "Balanced" suits most people.
  Memory    – how much of the conversation the model can keep in mind. Standard
              is fine; Large remembers more but uses more graphics memory.
  Speed     – the engine. "Turbo (auto-tuned)" is tuned to your GPU and is the
              recommended default; "Standard" is the plain engine.
  Images    – turn on if you want the model to look at pictures you paste.

Tip: pick a model and choose "Launch now" — the recommended settings already
fit your machine. Use "Customize" only if you want to change something.
'@
}

function Read-GuidedChoice {
    # Thin wrapper over Read-SpectreSelection: takes an ordered label->value map,
    # returns the chosen value (or $null when the user backs out). Interactive.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Message,
        [Parameter(Mandatory = $true)][System.Collections.Specialized.OrderedDictionary]$Map,
        [int]$PageSize = 10
    )
    $chosen = Read-SpectreSelection -Message $Message -Choices @($Map.Keys) -PageSize $PageSize
    if ($null -eq $chosen) { return $null }
    return $Map[$chosen]
}

function Invoke-GuidedCustomize {
    # Guided per-field editing in plain words. Returns the accumulated overrides
    # hashtable. The current value is shown on each "keep" option so the user always
    # sees what they have now.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][pscustomobject]$Plan
    )

    $overrides = @{}

    $targetMap = [ordered]@{}
    foreach ($t in @('localpilot', 'claude', 'codex', 'serve')) { $targetMap[(Get-GuidedTargetLabel -Value $t)] = $t }
    $targetMap["← keep current ($(Get-GuidedTargetLabel -Value $Plan.Target))"] = '__keep__'
    $t = Read-GuidedChoice -Message 'Run with which assistant?' -Map $targetMap
    if ($t -and $t -ne '__keep__') { $overrides.Target = $t }

    if ($Def.Contains('Quants')) {
        $qualityMap = [ordered]@{}
        foreach ($q in @($Def['Quants'].Keys | Sort-Object)) { $qualityMap[(Get-GuidedQualityLabel -Def $Def -Quant $q)] = $q }
        $qualityMap["← keep current ($(Get-GuidedQualityLabel -Def $Def -Quant $Plan.Quant))"] = '__keep__'
        $q = Read-GuidedChoice -Message 'Quality vs size?' -Map $qualityMap
        if ($q -and $q -ne '__keep__') { $overrides.Quant = $q }
    }

    if ($Def.Contains('Contexts')) {
        $memMap = [ordered]@{}
        foreach ($c in @($Def['Contexts'].Keys)) { $memMap[(Get-GuidedMemoryLabel -Def $Def -ContextKey $c)] = $c }
        $memMap["← keep current ($(Get-GuidedMemoryLabel -Def $Def -ContextKey $Plan.ContextKey))"] = '__keep__'
        $c = Read-GuidedChoice -Message 'How much memory (context)?' -Map $memMap
        if ($c -and $c -ne '__keep__') { $overrides.ContextKey = $c }
    }

    $imgMap = [ordered]@{ 'Off' = 'off'; 'On — understand images' = 'on'; '← keep current' = '__keep__' }
    $img = Read-GuidedChoice -Message 'Look at images?' -Map $imgMap
    if ($img -eq 'on') { $overrides.Vision = $true } elseif ($img -eq 'off') { $overrides.Vision = $false }

    return $overrides
}

function Start-LaunchGuided {
    # The friendly default: pick a model, confirm the recommended plan (or customize
    # it) in plain words, launch, then return to the menu when the agent exits.
    [CmdletBinding()]
    param([switch]$UseVision)

    if (-not (Get-Command Read-SpectreSelection -ErrorAction SilentlyContinue)) {
        Start-LLMWizardClassic -UseVision:$UseVision
        return
    }

    # Non-dev users shouldn't have to edit $PROFILE for Spectre's box-drawing to
    # render — ensure UTF-8 console output for this session before the first render.
    try {
        if ([Console]::OutputEncoding.CodePage -ne 65001) {
            [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
        }
    }
    catch { Write-Verbose $_.Exception.Message }

    $defaults = if ($script:Cfg -and $script:Cfg.Contains('DefaultLaunch')) { $script:Cfg['DefaultLaunch'] } else { @{} }

    while ($true) {
        $modelKey = Select-LLMModelKeySpectre
        if ([string]::IsNullOrWhiteSpace($modelKey)) { return }
        $def = Get-ModelDef -Key $modelKey
        $overrides = @{}
        if ($UseVision) { $overrides.Vision = $true }

        $launched = $false
        while (-not $launched) {
            $plan = Resolve-LaunchPlan -ModelKey $modelKey -Def $def -Defaults $defaults -Overrides $overrides
            Write-Host ""
            Write-Host (Format-GuidedPlanSummary -Plan $plan -Def $def) -ForegroundColor Gray
            Write-Host ""

            $menu = [ordered]@{
                '▶  Launch now (recommended settings)' = 'launch'
                '⚙  Customize settings'                 = 'customize'
                'ℹ  What do these mean?'                = 'help'
                '←  Back to models'                     = 'back'
            }
            $choice = Read-GuidedChoice -Message "Ready to launch $modelKey?" -Map $menu -PageSize 6
            switch ($choice) {
                'launch' {
                    $selArgs = Build-LaunchSelectionArgs -Plan $plan
                    try { Invoke-LLMSelection @selArgs } catch { Write-Warning "Launch failed: $($_.Exception.Message)" }
                    $launched = $true
                }
                'customize' {
                    $delta = Invoke-GuidedCustomize -Def $def -Plan $plan
                    foreach ($k in $delta.Keys) { $overrides[$k] = $delta[$k] }
                }
                'help' {
                    Write-Host ""
                    Write-Host (Get-GuidedGlossary) -ForegroundColor DarkGray
                    Write-Host ""
                }
                default { $launched = $true; $modelKey = $null }
            }
        }
        # Back to the model list (return-to-menu) unless the user quit the model picker.
    }
}
