using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using Terminal.Gui.App;
using Terminal.Gui.Drivers;
using Terminal.Gui.ViewBase;
using Terminal.Gui.Views;

var options = CliOptions.Parse(args);
var profilePath = ResolveLocalBoxProfilePath(args);
if (!File.Exists(profilePath))
{
    Console.Error.WriteLine($"LocalBox profile not found: {profilePath}");
    return 1;
}

var client = new PowerShellJsonClient(profilePath);
var showAllModels = false;
var models = await LoadModelsAsync(client, showAllModels);
var visibleModels = new List<LocalBoxModel>(models);
var status = await client.InvokeAsync<LocalBoxStatus>("Get-LocalBoxTuiStatus");
var benchPilot = await client.InvokeAsync<BenchPilotStatus>("Get-LocalBoxTuiBenchPilotStatus");
string? pendingLaunchCommand = null;
string? pendingShellCommand = null;

if (options.Check)
{
    Console.WriteLine($"LocalBox.Tui backend OK: {models.Count} models, {status?.VramGB ?? 0} GB VRAM ({status?.VramSource ?? "unknown"}), BenchPilot: {(benchPilot?.Available == true ? "available" : "unavailable")}");
    return 0;
}

using IApplication app = Application.Create();
app.Init();

using var window = new Window
{
    Title = WindowTitle(models.Count, visibleModels.Count, status, benchPilot, showAllModels),
    X = 0,
    Y = 0,
    Width = Dim.Fill(),
    Height = Dim.Fill()
};

var selectedContextIndex = 0;
var selectedActionIndex = 0;
var selectedModeIndex = 0;
var selectedAutoBestIndex = InitialAutoBestIndex(options.AutoBest);
var selectedQuantIndex = 0;
var strict = false;
var searchMode = false;
var searchTerm = "";
var step = WizardStep.Model;
var renderVersion = 0;
var actions = new[] { "claude", "codex", "unshackled", "remote", "chat", "setup", "findbest", "resetbest" };
var modes = new[] { "native", "turboquant", "mtpturbo" };
var autoBestChoices = new[] { "off", "auto", "balanced", "pure" };
var autoBestCache = new Dictionary<string, List<AutoBestProfile>>(StringComparer.OrdinalIgnoreCase);

var list = new ListView
{
    X = 0,
    Y = 0,
    Width = 38,
    Height = Dim.Fill(2)
};

#pragma warning disable CS0618
var detail = new TextView
{
    X = Pos.Right(list) + 1,
    Y = 0,
    Width = Dim.Fill(),
    Height = Dim.Fill(2),
    Text = "",
    ReadOnly = true,
    Multiline = true,
    WordWrap = false,
    CanFocus = true
};
#pragma warning restore CS0618

var footer = new Label
{
    X = 0,
    Y = Pos.Bottom(list),
    Width = Dim.Fill(),
    Height = 1,
    Text = ""
};

ApplyFilter();
SelectInitialModel();

LocalBoxModel? CurrentModel()
{
    var index = list.SelectedItem;
    if (index is null || index < 0 || index >= visibleModels.Count)
    {
        return null;
    }

    return visibleModels[index.Value];
}

void ApplyFilter()
{
    var query = searchTerm.Trim();
    visibleModels = string.IsNullOrWhiteSpace(query)
        ? [.. models]
        : [.. models.Where(m =>
            m.Key.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            m.DisplayName.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            m.Tier.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            m.SourceType.Contains(query, StringComparison.OrdinalIgnoreCase))];

    list.SetSource(new ObservableCollection<ModelRow>(visibleModels.Select(ModelRow.FromModel)));
    if (visibleModels.Count > 0)
    {
        list.SelectedItem = Math.Clamp(list.SelectedItem ?? 0, 0, visibleModels.Count - 1);
    }

    window.Title = WindowTitle(models.Count, visibleModels.Count, status, benchPilot, showAllModels);
}

void SelectInitialModel()
{
    if (string.IsNullOrWhiteSpace(options.Key))
    {
        return;
    }

    var index = visibleModels.FindIndex(m => m.Key.Equals(options.Key, StringComparison.OrdinalIgnoreCase));
    if (index >= 0)
    {
        list.SelectedItem = index;
        var model = visibleModels[index];
        selectedContextIndex = Math.Max(0, model.Contexts.FindIndex(c => c.Key.Equals(options.ContextKey, StringComparison.OrdinalIgnoreCase)));
        selectedAutoBestIndex = InitialAutoBestIndex(options.AutoBest);
    }
}

void ClampSelection()
{
    var model = CurrentModel();
    selectedContextIndex = Math.Clamp(selectedContextIndex, 0, Math.Max(model?.Contexts.Count ?? 0, 1) - 1);
    selectedQuantIndex = Math.Clamp(selectedQuantIndex, 0, Math.Max(model?.Quants.Count ?? 0, 1) - 1);
    selectedActionIndex = Math.Clamp(selectedActionIndex, 0, actions.Length - 1);
    selectedModeIndex = Math.Clamp(selectedModeIndex, 0, modes.Length - 1);
    selectedAutoBestIndex = Math.Clamp(selectedAutoBestIndex, 0, autoBestChoices.Length - 1);
}

string SelectedContextKey()
{
    var model = CurrentModel();
    if (model is null || model.Contexts.Count == 0)
    {
        return "";
    }

    ClampSelection();
    return model.Contexts[selectedContextIndex].Key;
}

string SelectedContextLabel()
{
    var model = CurrentModel();
    if (model is null || model.Contexts.Count == 0)
    {
        return "default";
    }

    ClampSelection();
    var ctx = model.Contexts[selectedContextIndex];
    return $"{ctx.Label}/{ctx.Tokens}";
}

string SelectedQuantLabel()
{
    var model = CurrentModel();
    if (model is null || model.Quants.Count == 0)
    {
        return "default";
    }

    ClampSelection();
    return model.Quants[selectedQuantIndex].Key;
}

string PlanExpression(string functionName)
{
    var model = CurrentModel() ?? throw new InvalidOperationException("No model selected.");
    var autoBest = autoBestChoices[selectedAutoBestIndex];
    var parts = new List<string>
    {
        functionName,
        "-Key", Ps(model.Key),
        "-ContextKey", Ps(SelectedContextKey()),
        "-Action", Ps(actions[selectedActionIndex]),
        "-Mode", Ps(modes[selectedModeIndex]),
        "-AutoBestProfile", Ps(autoBest == "off" ? "auto" : autoBest)
    };
    if (strict)
    {
        parts.Add("-Strict");
    }
    if (autoBest != "off")
    {
        parts.Add("-UseAutoBest");
    }
    return string.Join(" ", parts);
}

string AutoBestCacheKey(LocalBoxModel model)
{
    var quant = model.Quants.Count == 0 ? "" : model.Quants[selectedQuantIndex].Key;
    return $"{model.Key}|{SelectedContextKey()}|{modes[selectedModeIndex]}|{quant}";
}

void RenderFooter()
{
    ClampSelection();
    var filter = showAllModels ? "all" : "recommended";
    var search = searchMode ? $"search:{searchTerm}_" : $"search:{(string.IsNullOrWhiteSpace(searchTerm) ? "-" : searchTerm)}";
    footer.Text = $"step:{step} Enter/Right next Left back  F2 ctx:{SelectedContextLabel()} F3 action:{actions[selectedActionIndex]} F4 mode:{modes[selectedModeIndex]} F7 best:{autoBestChoices[selectedAutoBestIndex]} F8 strict:{strict} F10 {filter} F11 q:{SelectedQuantLabel()} / {search} F6 preview F9 launch Ctrl+B tune Tab focus Esc quit";
}

void RenderModel(int? index)
{
    renderVersion++;
    if (index is null || index < 0 || index >= visibleModels.Count)
    {
        detail.Text = "No model selected.";
        RenderFooter();
        return;
    }

    ClampSelection();
    var model = visibleModels[index.Value];
    var cacheKey = AutoBestCacheKey(model);
    var profiles = autoBestCache.TryGetValue(cacheKey, out var cachedProfiles) ? cachedProfiles : [];
    detail.Text = FormatModel(model, profiles, benchPilot, SelectedQuantLabel(), actions[selectedActionIndex], modes[selectedModeIndex]);
    RenderFooter();
    ScheduleAutoBestLoad(model, cacheKey, renderVersion);
}

void ScheduleAutoBestLoad(LocalBoxModel model, string cacheKey, int version)
{
    if (autoBestCache.ContainsKey(cacheKey))
    {
        return;
    }

    _ = Task.Run(async () =>
    {
        await Task.Delay(300);
        if (version != renderVersion)
        {
            return;
        }

        try
        {
            var parts = cacheKey.Split('|');
            var profiles = await client.InvokeArrayAsync<AutoBestProfile>($"Get-LocalBoxTuiAutoBestProfiles -Key {Ps(parts[0])} -ContextKey {Ps(parts[1])} -Mode {Ps(parts[2])} -Quant {Ps(parts[3])}");
            autoBestCache[cacheKey] = profiles;
            app.Invoke(() =>
            {
                if (version == renderVersion)
                {
                    RenderModel(list.SelectedItem);
                }
            });
        }
        catch
        {
            autoBestCache[cacheKey] = [];
        }
    });
}

void AdvanceStep()
{
    if (step == WizardStep.Confirm)
    {
        ShowPreview();
        return;
    }

    step = (WizardStep)((int)step + 1);
    RenderModel(list.SelectedItem);
}

void PreviousStep()
{
    if (step == WizardStep.Model)
    {
        list.SetFocus();
        return;
    }

    step = (WizardStep)((int)step - 1);
    RenderModel(list.SelectedItem);
}

void CycleCurrentStep()
{
    var model = CurrentModel();
    switch (step)
    {
        case WizardStep.Context when model is not null && model.Contexts.Count > 0:
            selectedContextIndex = (selectedContextIndex + 1) % model.Contexts.Count;
            break;
        case WizardStep.Quant when model is not null && model.Quants.Count > 0:
            selectedQuantIndex = (selectedQuantIndex + 1) % model.Quants.Count;
            break;
        case WizardStep.Action:
            selectedActionIndex = (selectedActionIndex + 1) % actions.Length;
            break;
        case WizardStep.Mode:
            selectedModeIndex = (selectedModeIndex + 1) % modes.Length;
            break;
        case WizardStep.AutoBest:
            selectedAutoBestIndex = (selectedAutoBestIndex + 1) % autoBestChoices.Length;
            break;
    }
    RenderModel(list.SelectedItem);
}

void ShowPreview()
{
    try
    {
        var preview = client.InvokeAsync<LaunchPreview>(PlanExpression("Invoke-LocalBoxTuiLaunchPreview")).GetAwaiter().GetResult();
        detail.Text = preview?.Output ?? "No preview output.";
        detail.SetFocus();
        RenderFooter();
    }
    catch (Exception ex)
    {
        detail.Text = ex.Message;
    }
}

list.ValueChanged += (_, args) =>
{
    selectedContextIndex = 0;
    selectedQuantIndex = 0;
    step = WizardStep.Model;
    RenderModel(args.NewValue);
};
list.Accepting += (_, _) => AdvanceStep();

window.KeyDown += (_, key) =>
{
    if (searchMode)
    {
        if (key.KeyCode == KeyCode.Enter)
        {
            searchMode = false;
        }
        else if (key.KeyCode == KeyCode.Esc)
        {
            searchMode = false;
            searchTerm = "";
            ApplyFilter();
        }
        else if (key.KeyCode == KeyCode.Backspace && searchTerm.Length > 0)
        {
            searchTerm = searchTerm[..^1];
            ApplyFilter();
        }
        else if (TryPrintable(key, out var ch))
        {
            searchTerm += ch;
            ApplyFilter();
        }

        RenderModel(list.SelectedItem);
        return;
    }

    if (key.KeyCode == KeyCode.Esc || key.KeyCode == (KeyCode.Q | KeyCode.CtrlMask))
    {
        app.RequestStop();
        return;
    }

    if (key.KeyCode == KeyCode.Enter || key.KeyCode == KeyCode.CursorRight)
    {
        AdvanceStep();
        return;
    }

    if (key.KeyCode == KeyCode.CursorLeft)
    {
        PreviousStep();
        return;
    }

    if (key.KeyCode == KeyCode.Space)
    {
        CycleCurrentStep();
        return;
    }

    if (key.KeyCode == (KeyCode)'/')
    {
        searchMode = true;
        RenderModel(list.SelectedItem);
        return;
    }

    if (key.KeyCode == KeyCode.F5)
    {
        try
        {
            models = LoadModelsAsync(client, showAllModels).GetAwaiter().GetResult();
            status = client.InvokeAsync<LocalBoxStatus>("Get-LocalBoxTuiStatus").GetAwaiter().GetResult();
            benchPilot = client.InvokeAsync<BenchPilotStatus>("Get-LocalBoxTuiBenchPilotStatus").GetAwaiter().GetResult();
            ApplyFilter();
            RenderModel(list.SelectedItem);
        }
        catch (Exception ex)
        {
            detail.Text = ex.Message;
        }
    }

    if (key.KeyCode == KeyCode.F2)
    {
        var model = CurrentModel();
        if (model is not null && model.Contexts.Count > 0)
        {
            selectedContextIndex = (selectedContextIndex + 1) % model.Contexts.Count;
            step = WizardStep.Context;
            RenderModel(list.SelectedItem);
        }
    }

    if (key.KeyCode == KeyCode.F3)
    {
        selectedActionIndex = (selectedActionIndex + 1) % actions.Length;
        step = WizardStep.Action;
        RenderModel(list.SelectedItem);
    }

    if (key.KeyCode == KeyCode.F4)
    {
        selectedModeIndex = (selectedModeIndex + 1) % modes.Length;
        step = WizardStep.Mode;
        RenderModel(list.SelectedItem);
    }

    if (key.KeyCode == KeyCode.F7)
    {
        selectedAutoBestIndex = (selectedAutoBestIndex + 1) % autoBestChoices.Length;
        step = WizardStep.AutoBest;
        RenderModel(list.SelectedItem);
    }

    if (key.KeyCode == KeyCode.F8)
    {
        strict = !strict;
        RenderModel(list.SelectedItem);
    }

    if (key.KeyCode == KeyCode.F10)
    {
        showAllModels = !showAllModels;
        try
        {
            models = LoadModelsAsync(client, showAllModels).GetAwaiter().GetResult();
            ApplyFilter();
            RenderModel(list.SelectedItem);
        }
        catch (Exception ex)
        {
            detail.Text = ex.Message;
        }
    }

    if (key.KeyCode == KeyCode.F11)
    {
        var model = CurrentModel();
        if (model is not null && model.Quants.Count > 0)
        {
            selectedQuantIndex = (selectedQuantIndex + 1) % model.Quants.Count;
            step = WizardStep.Quant;
            RenderModel(list.SelectedItem);
        }
    }

    if (key.KeyCode == (KeyCode.B | KeyCode.CtrlMask))
    {
        var model = CurrentModel();
        if (model is null)
        {
            detail.Text = "No model selected.";
            return;
        }

        if (benchPilot?.Available != true || string.IsNullOrWhiteSpace(benchPilot.Root))
        {
            detail.Text = $"BenchPilot is unavailable: {benchPilot?.Reason ?? "not found"}";
            return;
        }

        var project = Path.Combine(benchPilot.Root, "tui", "BenchPilot.Tui", "BenchPilot.Tui.csproj");
        pendingShellCommand = $"dotnet run --project {Ps(project)} -- --key {Ps(model.Key)} --context {Ps(SelectedContextKey())} --mode {Ps(modes[selectedModeIndex])}";
        app.RequestStop();
    }

    if (key.KeyCode == KeyCode.F6)
    {
        ShowPreview();
    }

    if (key.KeyCode == KeyCode.F9)
    {
        try
        {
            var plan = client.InvokeAsync<LaunchPlan>(PlanExpression("New-LocalBoxTuiLaunchPlan")).GetAwaiter().GetResult();
            pendingLaunchCommand = plan?.LaunchCommand;
            app.RequestStop();
        }
        catch (Exception ex)
        {
            detail.Text = ex.Message;
        }
    }
};

window.Add(list, detail, footer);
if (visibleModels.Count > 0)
{
    list.SelectedItem = Math.Clamp(list.SelectedItem ?? 0, 0, visibleModels.Count - 1);
    RenderModel(list.SelectedItem);
}
else
{
    RenderModel(null);
}

app.Run(window);
if (!string.IsNullOrWhiteSpace(pendingLaunchCommand))
{
    Console.WriteLine($"Launching: {pendingLaunchCommand}");
    await client.InvokeInteractiveAsync(pendingLaunchCommand);
}
if (!string.IsNullOrWhiteSpace(pendingShellCommand))
{
    Console.WriteLine($"Opening BenchPilot: {pendingShellCommand}");
    await RunShellCommandAsync(pendingShellCommand);
}
return 0;

static async Task<List<LocalBoxModel>> LoadModelsAsync(PowerShellJsonClient client, bool all)
{
    var expression = all ? "Get-LocalBoxTuiModels -All" : "Get-LocalBoxTuiModels";
    var selected = await client.InvokeArrayAsync<LocalBoxModel>(expression);
    if (selected.Count > 0 || all)
    {
        return selected;
    }

    return await client.InvokeArrayAsync<LocalBoxModel>("Get-LocalBoxTuiModels -All");
}

static string FormatModel(LocalBoxModel model, List<AutoBestProfile> profiles, BenchPilotStatus? benchPilot, string selectedQuant, string action, string mode)
{
    var sb = new StringBuilder();
    sb.AppendLine($"{model.Key} - {model.DisplayName}");
    sb.AppendLine();
    sb.AppendLine($"Tier        : {model.Tier}");
    sb.AppendLine($"Source      : {model.SourceType}");
    sb.AppendLine($"Parser      : {Empty(model.Parser)}");
    sb.AppendLine($"Default q   : {Empty(model.DefaultQuant)}");
    sb.AppendLine($"Selected q  : {Empty(selectedQuant)}");
    sb.AppendLine($"Action/mode : {action} / {mode}");
    sb.AppendLine($"Strict      : {model.Strict}");
    sb.AppendLine($"Limit tools : {model.LimitTools}");
    sb.AppendLine($"Vision      : {model.HasVision}");
    sb.AppendLine($"BenchPilot  : {(benchPilot?.Available == true ? $"available {benchPilot.Version}" : benchPilot?.Reason ?? "unknown")}");
    sb.AppendLine();

    if (!string.IsNullOrWhiteSpace(model.Description))
    {
        sb.AppendLine(model.Description);
        sb.AppendLine();
    }

    sb.AppendLine("Contexts");
    foreach (var ctx in model.Contexts)
    {
        var note = string.IsNullOrWhiteSpace(ctx.Note) ? "" : $" - {ctx.Note}";
        sb.AppendLine($"  {ctx.Label,-8} {ctx.Tokens,7} tokens{note}");
    }

    sb.AppendLine();
    sb.AppendLine("Quants");
    if (model.Quants.Count == 0)
    {
        sb.AppendLine("  none");
    }
    else
    {
        foreach (var q in model.Quants)
        {
            var size = q.SizeGB is null ? "?" : $"{q.SizeGB:0.0} GB";
            var current = q.IsDefault ? " default" : "";
            var selected = q.Key == selectedQuant ? " selected" : "";
            sb.AppendLine($"  {q.Key,-16} {size,8} {Empty(q.Fit),-6}{current}{selected}");
            if (!string.IsNullOrWhiteSpace(q.Note))
            {
                sb.AppendLine($"    {q.Note}");
            }
        }
    }

    sb.AppendLine();
    sb.AppendLine("AutoBest");
    if (profiles.Count == 0)
    {
        sb.AppendLine("  none for selected context/mode/quant");
    }
    else
    {
        foreach (var profile in profiles)
        {
            var stale = profile.StaleReasons.Count == 0 ? "" : $" stale: {string.Join(", ", profile.StaleReasons)}";
            sb.AppendLine($"  {profile.Profile,-8} {profile.Score,8:0.00} {profile.ScoreUnit} {profile.Quant} {profile.PromptLength}{stale}");
        }
    }

    sb.AppendLine();
    sb.AppendLine("Backends");
    foreach (var backend in model.BackendModes)
    {
        sb.AppendLine($"  {backend}");
    }

    return sb.ToString();
}

static string WindowTitle(int totalCount, int visibleCount, LocalBoxStatus? status, BenchPilotStatus? benchPilot, bool all)
{
    var modelScope = all ? "all" : "recommended";
    var bp = benchPilot?.Available == true ? $"BenchPilot {benchPilot.Version}" : "BenchPilot unavailable";
    return $"LocalBox.Tui - {visibleCount}/{totalCount} {modelScope} - {status?.VramGB ?? 0} GB VRAM ({status?.VramSource ?? "unknown"}) - {bp}";
}

static int InitialAutoBestIndex(string value)
{
    var choices = new[] { "off", "auto", "balanced", "pure" };
    var index = Array.FindIndex(choices, x => x.Equals(value, StringComparison.OrdinalIgnoreCase));
    return index < 0 ? 0 : index;
}

static bool TryPrintable(dynamic key, out char value)
{
    value = '\0';
    try
    {
        var rune = key.AsRune;
        if (rune.Value >= 32 && rune.Value < 127)
        {
            value = (char)rune.Value;
            return true;
        }
    }
    catch
    {
    }

    return false;
}

static string Empty(string? value) => string.IsNullOrWhiteSpace(value) ? "-" : value;

static string Ps(string value) => "'" + value.Replace("'", "''") + "'";

static string ResolveLocalBoxProfilePath(string[] args)
{
    for (var i = 0; i < args.Length - 1; i++)
    {
        if (args[i] is "--profile" or "--profile-path")
        {
            return Path.GetFullPath(args[i + 1]);
        }

        if (args[i] is "--root" or "-r")
        {
            var profile = FindProfileUnderRoot(args[i + 1]);
            if (!string.IsNullOrWhiteSpace(profile))
            {
                return profile;
            }
        }
    }

    var env = Environment.GetEnvironmentVariable("LOCALBOX_ROOT");
    if (!string.IsNullOrWhiteSpace(env))
    {
        var profile = FindProfileUnderRoot(env);
        if (!string.IsNullOrWhiteSpace(profile))
        {
            return profile;
        }
    }

    var envProfile = Environment.GetEnvironmentVariable("LOCALBOX_PROFILE");
    if (!string.IsNullOrWhiteSpace(envProfile))
    {
        return Path.GetFullPath(envProfile);
    }

    var installedSettings = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
        ".local-llm",
        "settings.json");
    if (File.Exists(installedSettings))
    {
        try
        {
            using var document = JsonDocument.Parse(File.ReadAllText(installedSettings));
            if (document.RootElement.TryGetProperty("LocalBoxRoot", out var localBoxRoot))
            {
                var configured = localBoxRoot.GetString();
                if (!string.IsNullOrWhiteSpace(configured))
                {
                    var profile = FindProfileUnderRoot(configured);
                    if (!string.IsNullOrWhiteSpace(profile))
                    {
                        return profile;
                    }
                }
            }
        }
        catch
        {
        }
    }

    var dir = new DirectoryInfo(Environment.CurrentDirectory);
    while (dir is not null)
    {
        var repoProfile = Path.Combine(dir.FullName, "local-llm", "LocalLLMProfile.ps1");
        if (File.Exists(repoProfile))
        {
            return repoProfile;
        }

        var installedProfile = Path.Combine(dir.FullName, "LocalLLMProfile.ps1");
        if (File.Exists(installedProfile))
        {
            return installedProfile;
        }

        dir = dir.Parent;
    }

    var homeProfile = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
        ".local-llm",
        "LocalLLMProfile.ps1");
    return homeProfile;
}

static string FindProfileUnderRoot(string root)
{
    var fullRoot = Path.GetFullPath(root);
    var repoProfile = Path.Combine(fullRoot, "local-llm", "LocalLLMProfile.ps1");
    if (File.Exists(repoProfile))
    {
        return repoProfile;
    }

    var installedProfile = Path.Combine(fullRoot, "LocalLLMProfile.ps1");
    if (File.Exists(installedProfile))
    {
        return installedProfile;
    }

    return "";
}

static async Task<int> RunShellCommandAsync(string expression)
{
    var encoded = Convert.ToBase64String(Encoding.Unicode.GetBytes(expression));
    using var process = Process.Start(new ProcessStartInfo
    {
        FileName = "pwsh",
        Arguments = $"-NoProfile -EncodedCommand {encoded}",
        UseShellExecute = false
    });
    if (process is null)
    {
        return 1;
    }

    await process.WaitForExitAsync();
    return process.ExitCode;
}

sealed class PowerShellJsonClient
{
    private readonly string profilePath;

    public PowerShellJsonClient(string profilePath)
    {
        this.profilePath = profilePath;
    }

    public async Task<T?> InvokeAsync<T>(string expression)
    {
        var json = await InvokeRawAsync(expression);
        if (string.IsNullOrWhiteSpace(json))
        {
            return default;
        }

        return JsonSerializer.Deserialize<T>(json, JsonOptions);
    }

    public async Task<List<T>> InvokeArrayAsync<T>(string expression)
    {
        var json = await InvokeRawAsync(expression);
        if (string.IsNullOrWhiteSpace(json))
        {
            return [];
        }

        using var document = JsonDocument.Parse(json);
        if (document.RootElement.ValueKind == JsonValueKind.Array)
        {
            return JsonSerializer.Deserialize<List<T>>(json, JsonOptions) ?? [];
        }

        var item = document.RootElement.Deserialize<T>(JsonOptions);
        return item is null ? [] : [item];
    }

    private async Task<string> InvokeRawAsync(string expression)
    {
        var escapedProfile = profilePath.Replace("'", "''");
        var command = "$ErrorActionPreference = 'Stop'; " +
                      "$env:LOCALBOX_SKIP_PROXY_CHECK = '1'; " +
                      $". '{escapedProfile}'; " +
                      $"@({expression}) | ConvertTo-Json -Depth 16 -Compress";
        var encoded = Convert.ToBase64String(Encoding.Unicode.GetBytes(command));

        using var process = new Process();
        process.StartInfo = new ProcessStartInfo
        {
            FileName = "pwsh",
            Arguments = $"-NoProfile -NonInteractive -EncodedCommand {encoded}",
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };

        process.Start();
        var stdoutTask = process.StandardOutput.ReadToEndAsync();
        var stderrTask = process.StandardError.ReadToEndAsync();
        await process.WaitForExitAsync();

        var stdout = await stdoutTask;
        var stderr = await stderrTask;
        if (process.ExitCode != 0)
        {
            throw new InvalidOperationException(stderr.Trim().Length > 0 ? stderr.Trim() : $"PowerShell exited {process.ExitCode}.");
        }

        return stdout.Trim();
    }

    public async Task<int> InvokeInteractiveAsync(string expression)
    {
        var escapedProfile = profilePath.Replace("'", "''");
        var command = "$ErrorActionPreference = 'Stop'; " +
                      "$env:LOCALBOX_SKIP_PROXY_CHECK = '1'; " +
                      $". '{escapedProfile}'; " +
                      expression;
        var encoded = Convert.ToBase64String(Encoding.Unicode.GetBytes(command));

        using var process = new Process();
        process.StartInfo = new ProcessStartInfo
        {
            FileName = "pwsh",
            Arguments = $"-NoProfile -EncodedCommand {encoded}",
            UseShellExecute = false
        };

        process.Start();
        await process.WaitForExitAsync();
        return process.ExitCode;
    }

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNameCaseInsensitive = true
    };
}

sealed record CliOptions(string Key, string ContextKey, string AutoBest, bool Check)
{
    public static CliOptions Parse(string[] args)
    {
        return new CliOptions(
            Value(args, "--key") ?? "",
            Value(args, "--context") ?? Value(args, "--context-key") ?? "",
            Value(args, "--autobest") ?? "off",
            args.Any(a => a.Equals("--check", StringComparison.OrdinalIgnoreCase)));
    }

    private static string? Value(string[] args, string name)
    {
        for (var i = 0; i < args.Length - 1; i++)
        {
            if (args[i].Equals(name, StringComparison.OrdinalIgnoreCase))
            {
                return args[i + 1];
            }
        }

        return null;
    }
}

sealed record ModelRow(string Key, string DisplayName, string Tier)
{
    public static ModelRow FromModel(LocalBoxModel model) => new(model.Key, model.DisplayName, model.Tier);

    public override string ToString()
    {
        var name = DisplayName.Length > 18 ? DisplayName[..18] : DisplayName;
        return $"{Key,-14} {Tier,-10} {name}";
    }
}

enum WizardStep
{
    Model,
    Context,
    Quant,
    Action,
    Mode,
    AutoBest,
    Confirm
}

sealed record LocalBoxStatus
{
    public int VramGB { get; init; }
    public string VramSource { get; init; } = "";
}

sealed record BenchPilotStatus
{
    public bool Available { get; init; }
    public string Reason { get; init; } = "";
    public string Version { get; init; } = "";
    public string Root { get; init; } = "";
}

sealed record LaunchPlan
{
    public string LaunchCommand { get; init; } = "";
}

sealed record LaunchPreview
{
    public string Command { get; init; } = "";
    public string Output { get; init; } = "";
}

sealed record AutoBestProfile
{
    public string Profile { get; init; } = "";
    public double Score { get; init; }
    public string ScoreUnit { get; init; } = "";
    public string Quant { get; init; } = "";
    public string PromptLength { get; init; } = "";
    public List<string> StaleReasons { get; init; } = [];
}

sealed record LocalBoxModel
{
    public string Key { get; init; } = "";
    public string DisplayName { get; init; } = "";
    public string Description { get; init; } = "";
    public string Tier { get; init; } = "";
    public string SourceType { get; init; } = "";
    public string Parser { get; init; } = "";
    public string DefaultQuant { get; init; } = "";
    public bool Strict { get; init; }
    public bool LimitTools { get; init; }
    public bool HasVision { get; init; }
    public List<LocalBoxContext> Contexts { get; init; } = [];
    public List<LocalBoxQuant> Quants { get; init; } = [];
    public List<string> BackendModes { get; init; } = [];
}

sealed record LocalBoxContext
{
    public string Key { get; init; } = "";
    public string Label { get; init; } = "";
    public int Tokens { get; init; }
    public string Note { get; init; } = "";
    public bool IsDefault { get; init; }
}

sealed record LocalBoxQuant
{
    public string Key { get; init; } = "";
    public string File { get; init; } = "";
    public double? SizeGB { get; init; }
    public string Fit { get; init; } = "";
    public string Note { get; init; } = "";
    public bool IsDefault { get; init; }
}
