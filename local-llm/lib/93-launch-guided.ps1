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
    $kv = if ($Plan.KvCacheK) { if ($Plan.KvCacheK -eq $Plan.KvCacheV) { $Plan.KvCacheK } else { '{0}/{1}' -f $Plan.KvCacheK, $Plan.KvCacheV } }
    elseif ($Plan.UseAutoBest) { 'chosen by auto-tune' }
    else { 'auto (default)' }
    @(
        ('Model:     {0}' -f $name)
        ('Run with:  {0}' -f (Get-GuidedTargetLabel -Value $Plan.Target))
        ('Quality:   {0} · {1}' -f (Get-GuidedQualityLabel -Def $Def -Quant $Plan.Quant), $Plan.Quant)
        ('Memory:    {0}' -f (Get-GuidedMemoryLabel -Def $Def -ContextKey $Plan.ContextKey))
        ('Speed:     {0}' -f $speed)
        ('KV cache:  {0}' -f $kv)
        ('Images:    {0}   ·   Strict: {1}' -f $(if ($Plan.Vision) { 'on' } else { 'off' }), $(if ($Plan.Strict) { 'on' } else { 'off' }))
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

Auto-tune  – "Auto-tune this model (run a benchmark)" measures your GPU once and
             saves the fastest safe settings (engine + KV cache). After that,
             leaving Auto-tune "on" uses those saved results automatically.

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
    # Full settings editor: a menu of every setting with its current value; choosing
    # one opens the detailed Spectre picker (quant with fit badges, KV cache, engine,
    # context, …); "Save these as my default" persists the choices; "Done" returns.
    # Returns the accumulated overrides. Reuses the existing Spectre pickers so nothing
    # is hidden from a power user.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$ModelKey,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [System.Collections.IDictionary]$Defaults = @{}
    )

    $overrides = @{}
    while ($true) {
        $plan = Resolve-LaunchPlan -ModelKey $ModelKey -Def $Def -Defaults $Defaults -Overrides $overrides
        $autoLabel = if ($plan.UseAutoBest) { '{0} (on)' -f $plan.AutoBestProfile } else { 'off (manual)' }
        $ctxLabel = if ($plan.ContextKey) { $plan.ContextKey } else { 'default' }

        $menu = [ordered]@{}
        $menu[('Run with:         {0}' -f (Get-GuidedTargetLabel -Value $plan.Target))] = 'target'
        if ($Def.Contains('Quants')) { $menu[('Quality (quant):  {0}' -f $plan.Quant)] = 'quant' }
        $menu[('Memory (context): {0}' -f $ctxLabel)] = 'context'
        # Engine and KV cache are only user choices in manual mode; when auto-tune is
        # on it selects/tunes them, so they show as auto-tuned and aren't editable here.
        if (-not $plan.UseAutoBest) {
            $menu[('Engine (mode):    {0}' -f $plan.Mode)] = 'mode'
        }
        else {
            $menu[('Engine (mode):    {0}  (auto-tuned)' -f $plan.Mode)] = 'mode-locked'
        }
        $menu[('Auto-tune:        {0}' -f $autoLabel)] = 'autobest'
        if (-not $plan.UseAutoBest) {
            $kvLabel = if ($plan.KvCacheK) { if ($plan.KvCacheK -eq $plan.KvCacheV) { $plan.KvCacheK } else { '{0}/{1}' -f $plan.KvCacheK, $plan.KvCacheV } } else { 'auto (default)' }
            $menu[('KV cache:         {0}' -f $kvLabel)] = 'kv'
        }
        else {
            $menu['KV cache:         (chosen by auto-tune)'] = 'kv-locked'
        }
        $menu[('Images (vision):  {0}' -f $(if ($plan.Vision) { 'on' } else { 'off' }))] = 'vision'
        $menu[('Strict output:    {0}' -f $(if ($plan.Strict) { 'on' } else { 'off' }))] = 'strict'
        $menu['— Save these as my default —'] = 'save'
        $menu['✓  Done — back to launch'] = 'done'

        $choice = Read-GuidedChoice -Message "Settings — $ModelKey" -Map $menu -PageSize 14
        switch ($choice) {
            'target' { $v = Select-LLMActionSpectre; if ($v -in @('localpilot', 'claude', 'codex', 'serve')) { $overrides.Target = $v } }
            'quant' { $v = Select-LLMQuantKeySpectre -ModelKey $ModelKey; if ($v -and $v -ne '__keep__') { $overrides.Quant = $v } }
            'context' { $v = Select-LLMContextKeySpectre -ModelKey $ModelKey; if ($null -ne $v) { $overrides.ContextKey = $v } }
            'mode' { $v = Select-LLMModeSpectre; if ($v) { $overrides.Mode = $v } }
            'mode-locked' { Write-Host 'Auto-tune selects and tunes the engine for you. Turn Auto-tune off to choose it yourself.' -ForegroundColor DarkGray }
            'autobest' {
                $abMap = [ordered]@{ 'Off — I will set things manually' = 'off'; 'Auto' = 'auto'; 'Balanced (recommended)' = 'balanced'; 'Pure — max speed' = 'pure' }
                $ab = Read-GuidedChoice -Message 'Auto-tune settings for your GPU?' -Map $abMap -PageSize 6
                if ($ab -eq 'off') { $overrides.UseAutoBest = $false }
                elseif ($ab) {
                    $overrides.UseAutoBest = $true
                    $overrides.AutoBestProfile = $ab
                    # Auto-tune owns the KV cache — drop any manual override so it isn't stranded.
                    $overrides.Remove('KvCacheK'); $overrides.Remove('KvCacheV')
                }
            }
            'kv' { $kv = Select-LLMKvCacheSpectre -Mode $plan.Mode; if ($kv) { $overrides.KvCacheK = $kv.K; $overrides.KvCacheV = $kv.V } }
            'kv-locked' { Write-Host 'Auto-tune chooses the KV cache for you. Turn Auto-tune off to set it yourself.' -ForegroundColor DarkGray }
            'vision' { $overrides.Vision = (-not $plan.Vision) }
            'strict' { $overrides.Strict = (-not $plan.Strict) }
            'save' {
                if ($plan.Target -in @('localpilot', 'claude', 'codex')) {
                    Save-LLMDefaultLaunch -ModelKey $ModelKey -ContextKey $plan.ContextKey -Action $plan.Target `
                        -LlamaCppMode $plan.Mode -KvCacheK $plan.KvCacheK -KvCacheV $plan.KvCacheV `
                        -Strict:$plan.Strict -UseAutoBest:$plan.UseAutoBest -AutoBestProfile $plan.AutoBestProfile
                }
                else {
                    Write-Host "'$(Get-GuidedTargetLabel -Value $plan.Target)' can't be saved as a default launch target." -ForegroundColor Yellow
                }
            }
            'done' { return $overrides }
            default { return $overrides }
        }
    }
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
                '▶  Launch now (recommended settings)'          = 'launch'
                '⚙  Customize settings'                          = 'customize'
                '🔧  Auto-tune this model (run a benchmark)'      = 'tune'
                'ℹ  What do these mean?'                         = 'help'
                '←  Back to models'                              = 'back'
            }
            $choice = Read-GuidedChoice -Message "Ready to launch $modelKey?" -Map $menu -PageSize 6
            switch ($choice) {
                'launch' {
                    $selArgs = Build-LaunchSelectionArgs -Plan $plan
                    try { Invoke-LLMSelection @selArgs } catch { Write-Warning "Launch failed: $($_.Exception.Message)" }
                    $launched = $true
                }
                'customize' {
                    $delta = Invoke-GuidedCustomize -ModelKey $modelKey -Def $def -Defaults $defaults
                    foreach ($k in $delta.Keys) { $overrides[$k] = $delta[$k] }
                }
                'tune' {
                    # Run the tuner (benchmark) for this model + engine + memory; it saves
                    # the best settings, which Auto-tune then uses on the next launch.
                    Write-Host "`nRunning a benchmark to auto-tune $modelKey for your GPU — this can take a few minutes." -ForegroundColor Cyan
                    try {
                        Invoke-LLMSelection -Action findbest -ModelKey $modelKey -ContextKey $plan.ContextKey -LlamaCppMode $plan.Mode
                    }
                    catch { Write-Warning "Auto-tune failed: $($_.Exception.Message)" }
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
