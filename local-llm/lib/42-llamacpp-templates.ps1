# Parser → llama-server flags. Two responsibilities:
# 1. Map a parser name (qwen3coder, qwen36, etc.) to a `--chat-template` or
#    `--chat-template-file` argument set.
# 2. Translate the PARAMETER lines from Get-ParserLines (40-parsers.ps1) into
#    the equivalent llama-server CLI flags so sampling stays consistent.

function Resolve-LlamaCppChatTemplate {
    # Returns a [string[]] of CLI args (empty when the model's GGUF metadata
    # already carries a usable template). Honors a per-model ChatTemplate
    # override that, if set, wins over the parser-based mapping.
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Parser,
        [string]$Override
    )

    if (-not [string]::IsNullOrWhiteSpace($Override)) {
        if (Test-Path $Override) {
            return @('--chat-template-file', $Override)
        }
        return @('--chat-template', $Override)
    }

    switch ($Parser) {
        'none'           { return @() }
        'qwen3coder'     { return @('--jinja') }
        'qwen36'         { return @('--jinja') }
        'qwen36-think'   { return @('--jinja') }
        default          { return @() }
    }
}

function Get-LlamaCppReasoningArgs {
    # Maps the catalog ThinkingPolicy to llama-server reasoning flags.
    # `strip` must disable reasoning generation, not just hide it on the wire:
    # otherwise Qwen thinking templates can spend minutes generating invisible
    # <think> tokens before producing a user-visible answer.
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ThinkingPolicy
    )

    $policy = if ([string]::IsNullOrWhiteSpace($ThinkingPolicy)) { 'strip' } else { $ThinkingPolicy }

    if ($policy -eq 'keep') {
        return @('--reasoning', 'on', '--reasoning-format', 'deepseek')
    }

    return @(
        '--reasoning', 'off',
        '--reasoning-budget', '0',
        '--reasoning-format', 'none'
    )
}

function ConvertFrom-OllamaParameter {
    # Reads PARAMETER lines from Get-ParserLines (40-parsers.ps1) and emits the
    # equivalent llama-server CLI flags. Unknown PARAMETER names are skipped
    # silently. Name retained from the Modelfile-era parser format the
    # PARAMETER lines still use.
    param([Parameter(Mandatory = $true)][System.Collections.IEnumerable]$Lines)

    $out = New-Object System.Collections.Generic.List[string]

    foreach ($line in $Lines) {
        $text = [string]$line
        if (-not $text) { continue }
        if ($text -notmatch '^\s*PARAMETER\s+(\S+)\s+(.+)\s*$') { continue }

        $name = $Matches[1].ToLowerInvariant()
        $value = $Matches[2].Trim()

        # Unwrap one layer of surrounding quotes (single or double).
        if ($value.Length -ge 2 -and ($value[0] -in @('"', "'")) -and $value[-1] -eq $value[0]) {
            $value = $value.Substring(1, $value.Length - 2)
        }

        switch ($name) {
            'temperature'      { $out.Add('--temp');             $out.Add($value); break }
            'top_k'            { $out.Add('--top-k');            $out.Add($value); break }
            'top_p'            { $out.Add('--top-p');            $out.Add($value); break }
            'min_p'            { $out.Add('--min-p');            $out.Add($value); break }
            'repeat_penalty'   { $out.Add('--repeat-penalty');   $out.Add($value); break }
            'repeat_last_n'    { $out.Add('--repeat-last-n');    $out.Add($value); break }
            'presence_penalty' { $out.Add('--presence-penalty'); $out.Add($value); break }
            'frequency_penalty'{ $out.Add('--frequency-penalty');$out.Add($value); break }
            'tfs_z'            { $out.Add('--tfs');              $out.Add($value); break }
            'typical_p'        { $out.Add('--typical');          $out.Add($value); break }
            'mirostat'         { $out.Add('--mirostat');         $out.Add($value); break }
            'mirostat_tau'     { $out.Add('--mirostat-ent');     $out.Add($value); break }
            'mirostat_eta'     { $out.Add('--mirostat-lr');      $out.Add($value); break }
            'seed'             { $out.Add('--seed');             $out.Add($value); break }
            # PARAMETER stop / num_ctx / num_predict are handled elsewhere.
            default            { }
        }
    }

    return @($out)
}

function Get-LlamaCppStrictSamplerArgs {
    # The strict overlay's PARAMETER values, translated for llama-server.
    return (ConvertFrom-OllamaParameter -Lines (Get-StrictModelfileLines))
}
