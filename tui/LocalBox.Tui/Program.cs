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
var activePane = ActivePane.Models;
var pickerOpen = false;
var pickerStep = WizardStep.Context;
var pickerIndex = 0;
var pickerChoices = new List<PickerChoice>();
var renderVersion = 0;
var actions = new[] { "claude", "codex", "unshackled", "remote", "chat", "setup", "findbest", "resetbest" };
var modes = new[] { "native", "turboquant", "mtpturbo" };
var autoBestChoices = new[] { "off", "auto", "balanced", "pure" };
var autoBestCache = new Dictionary<string, List<AutoBestProfile>>(StringComparer.OrdinalIgnoreCase);

var list = new ListView
{
    X = 0,
    Y = 0,
    Width = 34,
    Height = Dim.Fill(2)
};

var wizard = new ListView
{
    X = Pos.Right(list) + 1,
    Y = 0,
    Width = 34,
    Height = Dim.Fill(2)
};

#pragma warning disable CS0618
var detail = new TextView
{
    X = Pos.Right(wizard) + 1,
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

void FocusPane(ActivePane pane)
{
    activePane = pane;
    switch (pane)
    {
        case ActivePane.Models:
            pickerOpen = false;
            list.SetFocus();
            break;
        case ActivePane.Wizard:
            pickerOpen = false;
            wizard.SetFocus();
            break;
        case ActivePane.Choices:
            detail.SetFocus();
            break;
    }

    RenderModel(list.SelectedItem);
}

void RenderFooter()
{
    ClampSelection();
    if (pickerOpen)
    {
        footer.Text = $"pane:choices select:{pickerStep} Up/Down choose Enter accept Left/Backspace/Esc back";
        return;
    }

    var filter = showAllModels ? "all" : "recommended";
    var search = searchMode ? $"search:{searchTerm}_" : $"search:{(string.IsNullOrWhiteSpace(searchTerm) ? "-" : searchTerm)}";
    footer.Text = activePane switch
    {
        ActivePane.Models => $"pane:models Up/Down model Enter/Right wizard  F10 {filter} / {search} Esc quit",
        ActivePane.Wizard => $"pane:wizard Up/Down field Enter choices Right next Left models  P preview L launch S strict:{strict} Esc quit",
        _ => $"pane:{activePane} Enter select Right next Left back  F10 {filter} / {search} Esc quit"
    };
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
    RenderWizard();
    detail.Text = FormatModel(
        model,
        profiles,
        benchPilot,
        step,
        SelectedContextKey(),
        SelectedContextLabel(),
        SelectedQuantLabel(),
        actions[selectedActionIndex],
        modes[selectedModeIndex],
        autoBestChoices[selectedAutoBestIndex],
        strict);
    RenderFooter();
    ScheduleAutoBestLoad(model, cacheKey, renderVersion);
}

void RenderWizard()
{
    ClampSelection();
    var rows = new ObservableCollection<WizardRow>
    {
        new(WizardStep.Model, "Model", CurrentModel()?.Key ?? "-"),
        new(WizardStep.Context, "Context", SelectedContextLabel()),
        new(WizardStep.Quant, "Quant", SelectedQuantLabel()),
        new(WizardStep.Action, "Action", actions[selectedActionIndex]),
        new(WizardStep.Mode, "Mode", modes[selectedModeIndex]),
        new(WizardStep.AutoBest, "AutoBest", autoBestChoices[selectedAutoBestIndex]),
        new(WizardStep.Confirm, "Confirm", "launch")
    };
    wizard.SetSource(rows);
    wizard.SelectedItem = Math.Clamp((int)step, 0, rows.Count - 1);
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
        LaunchSelected();
        return;
    }

    step = (WizardStep)((int)step + 1);
    RenderModel(list.SelectedItem);
}

void PreviousStep()
{
    if (activePane == ActivePane.Choices)
    {
        CancelPicker();
        return;
    }

    if (activePane == ActivePane.Wizard)
    {
        FocusPane(ActivePane.Models);
        return;
    }

    FocusPane(ActivePane.Models);
}

void MoveWizard(int delta)
{
    var next = Math.Clamp((int)step + delta, (int)WizardStep.Model, (int)WizardStep.Confirm);
    step = (WizardStep)next;
    RenderModel(list.SelectedItem);
}

void OpenOrAcceptStep()
{
    if (step == WizardStep.Confirm)
    {
        LaunchSelected();
        return;
    }

    if (StepHasPicker(step))
    {
        OpenPicker(step);
        return;
    }

    AdvanceStep();
}

bool StepHasPicker(WizardStep candidate)
{
    return candidate is WizardStep.Context or WizardStep.Quant or WizardStep.Action or WizardStep.Mode or WizardStep.AutoBest;
}

void OpenPicker(WizardStep targetStep)
{
    var model = CurrentModel();
    FocusPane(ActivePane.Wizard);
    step = targetStep;
    ClampSelection();

    pickerChoices = BuildPickerChoices(targetStep, model);
    if (pickerChoices.Count == 0)
    {
        RenderModel(list.SelectedItem);
        return;
    }

    pickerStep = targetStep;
    pickerIndex = CurrentPickerIndex(targetStep);
    pickerOpen = true;
    activePane = ActivePane.Choices;
    RenderPicker();
}

List<PickerChoice> BuildPickerChoices(WizardStep targetStep, LocalBoxModel? model)
{
    var choices = new List<PickerChoice>();
    switch (targetStep)
    {
        case WizardStep.Context when model is not null && model.Contexts.Count > 0:
            for (var i = 0; i < model.Contexts.Count; i++)
            {
                var index = i;
                var ctx = model.Contexts[i];
                var note = string.IsNullOrWhiteSpace(ctx.Note) ? "" : $" - {ctx.Note}";
                choices.Add(new PickerChoice($"{ctx.Label,-8} {ctx.Tokens,7} tokens{note}", () => selectedContextIndex = index));
            }
            break;
        case WizardStep.Quant when model is not null && model.Quants.Count > 0:
            for (var i = 0; i < model.Quants.Count; i++)
            {
                var index = i;
                var quant = model.Quants[i];
                var size = quant.SizeGB is null ? "?" : $"{quant.SizeGB:0.0} GB";
                var fit = Empty(quant.Fit);
                var current = quant.IsDefault ? " default" : "";
                var note = string.IsNullOrWhiteSpace(quant.Note) ? "" : $" - {quant.Note}";
                choices.Add(new PickerChoice($"{quant.Key,-16} {size,8} {fit,-6}{current}{note}", () => selectedQuantIndex = index));
            }
            break;
        case WizardStep.Action:
            for (var i = 0; i < actions.Length; i++)
            {
                var index = i;
                choices.Add(new PickerChoice(actions[i], () => selectedActionIndex = index));
            }
            break;
        case WizardStep.Mode:
            for (var i = 0; i < modes.Length; i++)
            {
                var index = i;
                choices.Add(new PickerChoice(modes[i], () => selectedModeIndex = index));
            }
            break;
        case WizardStep.AutoBest:
            for (var i = 0; i < autoBestChoices.Length; i++)
            {
                var index = i;
                choices.Add(new PickerChoice(autoBestChoices[i], () => selectedAutoBestIndex = index));
            }
            break;
    }

    return choices;
}

int CurrentPickerIndex(WizardStep targetStep)
{
    return targetStep switch
    {
        WizardStep.Context => selectedContextIndex,
        WizardStep.Quant => selectedQuantIndex,
        WizardStep.Action => selectedActionIndex,
        WizardStep.Mode => selectedModeIndex,
        WizardStep.AutoBest => selectedAutoBestIndex,
        _ => 0
    };
}

void RenderPicker()
{
    pickerIndex = Math.Clamp(pickerIndex, 0, Math.Max(pickerChoices.Count, 1) - 1);
    detail.Text = FormatPicker(pickerStep, pickerChoices, pickerIndex);
    RenderFooter();
}

void MovePicker(int delta)
{
    if (pickerChoices.Count == 0)
    {
        return;
    }

    pickerIndex = (pickerIndex + delta + pickerChoices.Count) % pickerChoices.Count;
    RenderPicker();
}

void AcceptPicker()
{
    if (pickerChoices.Count > 0)
    {
        pickerChoices[pickerIndex].Apply();
    }

    pickerOpen = false;
    pickerChoices = [];
    activePane = ActivePane.Wizard;
    wizard.SetFocus();
    RenderModel(list.SelectedItem);
}

void CancelPicker()
{
    pickerOpen = false;
    pickerChoices = [];
    activePane = ActivePane.Wizard;
    wizard.SetFocus();
    RenderModel(list.SelectedItem);
}

void LaunchSelected()
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

void ShowPreview()
{
    try
    {
        var preview = client.InvokeAsync<LaunchPreview>(PlanExpression("Invoke-LocalBoxTuiLaunchPreview")).GetAwaiter().GetResult();
        detail.Text = preview?.Output ?? "No preview output.";
        RenderFooter();
    }
    catch (Exception ex)
    {
        detail.Text = ex.Message;
    }
}

void HandleKey(dynamic key)
{
    if (pickerOpen)
    {
        if (key.KeyCode == KeyCode.Enter)
        {
            AcceptPicker();
        }
        else if (key.KeyCode == KeyCode.Esc || key.KeyCode == KeyCode.CursorLeft || key.KeyCode == KeyCode.Backspace)
        {
            CancelPicker();
        }
        else if (key.KeyCode == KeyCode.CursorDown || IsKey(key, 'j'))
        {
            MovePicker(1);
        }
        else if (key.KeyCode == KeyCode.CursorUp || IsKey(key, 'k'))
        {
            MovePicker(-1);
        }
        else if (key.KeyCode == KeyCode.Home)
        {
            pickerIndex = 0;
            RenderPicker();
        }
        else if (key.KeyCode == KeyCode.End)
        {
            pickerIndex = pickerChoices.Count - 1;
            RenderPicker();
        }

        key.Handled = true;
        return;
    }

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
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.Esc || key.KeyCode == (KeyCode.Q | KeyCode.CtrlMask))
    {
        app.RequestStop();
        key.Handled = true;
        return;
    }

    if (activePane == ActivePane.Models)
    {
        if (key.KeyCode == KeyCode.Enter || key.KeyCode == KeyCode.CursorRight)
        {
            step = WizardStep.Context;
            FocusPane(ActivePane.Wizard);
            key.Handled = true;
            return;
        }
    }

    if (activePane == ActivePane.Wizard)
    {
        if (key.KeyCode == KeyCode.CursorUp || IsKey(key, 'k'))
        {
            MoveWizard(-1);
            key.Handled = true;
            return;
        }

        if (key.KeyCode == KeyCode.CursorDown || IsKey(key, 'j'))
        {
            MoveWizard(1);
            key.Handled = true;
            return;
        }

        if (key.KeyCode == KeyCode.Home)
        {
            step = WizardStep.Model;
            RenderModel(list.SelectedItem);
            key.Handled = true;
            return;
        }

        if (key.KeyCode == KeyCode.End)
        {
            step = WizardStep.Confirm;
            RenderModel(list.SelectedItem);
            key.Handled = true;
            return;
        }

        if (key.KeyCode == KeyCode.Enter)
        {
            OpenOrAcceptStep();
            key.Handled = true;
            return;
        }

        if (key.KeyCode == KeyCode.CursorRight)
        {
            AdvanceStep();
            key.Handled = true;
            return;
        }
    }

    if (key.KeyCode == KeyCode.CursorLeft || key.KeyCode == KeyCode.Backspace)
    {
        PreviousStep();
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.Space)
    {
        if (activePane == ActivePane.Wizard && StepHasPicker(step))
        {
            OpenPicker(step);
            key.Handled = true;
        }
        return;
    }

    if (key.KeyCode == (KeyCode)'/')
    {
        searchMode = true;
        RenderModel(list.SelectedItem);
        key.Handled = true;
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
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F2 || IsKey(key, 'c'))
    {
        OpenPicker(WizardStep.Context);
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F3 || IsKey(key, 'a'))
    {
        OpenPicker(WizardStep.Action);
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F4 || IsKey(key, 'm'))
    {
        OpenPicker(WizardStep.Mode);
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F7 || IsKey(key, 'b'))
    {
        OpenPicker(WizardStep.AutoBest);
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F8 || IsKey(key, 's'))
    {
        strict = !strict;
        RenderModel(list.SelectedItem);
        key.Handled = true;
        return;
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
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F11 || IsKey(key, 'q'))
    {
        OpenPicker(WizardStep.Quant);
        key.Handled = true;
        return;
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
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F6 || IsKey(key, 'p'))
    {
        ShowPreview();
        key.Handled = true;
        return;
    }

    if (key.KeyCode == KeyCode.F9 || IsKey(key, 'l'))
    {
        LaunchSelected();
        key.Handled = true;
        return;
    }
}

list.ValueChanged += (_, args) =>
{
    pickerOpen = false;
    pickerChoices = [];
    selectedContextIndex = 0;
    selectedQuantIndex = 0;
    step = WizardStep.Model;
    RenderModel(args.NewValue);
};
list.Accepting += (_, _) =>
{
    step = WizardStep.Context;
    FocusPane(ActivePane.Wizard);
};

app.Keyboard.KeyDown += (_, key) => HandleKey(key);

window.Add(list, wizard, detail, footer);
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

static string FormatModel(
    LocalBoxModel model,
    List<AutoBestProfile> profiles,
    BenchPilotStatus? benchPilot,
    WizardStep step,
    string selectedContextKey,
    string selectedContextLabel,
    string selectedQuant,
    string action,
    string mode,
    string autoBest,
    bool strict)
{
    var sb = new StringBuilder();
    sb.AppendLine($"{model.Key} - {model.DisplayName}");
    sb.AppendLine();
    sb.AppendLine($"Tier        : {model.Tier}");
    sb.AppendLine($"Source      : {model.SourceType}");
    sb.AppendLine($"Parser      : {Empty(model.Parser)}");
    sb.AppendLine($"Default q   : {Empty(model.DefaultQuant)}");
    sb.AppendLine($"Context     : {Empty(selectedContextLabel)}");
    sb.AppendLine($"Selected q  : {Empty(selectedQuant)}");
    sb.AppendLine($"Action/mode : {action} / {mode}");
    sb.AppendLine($"AutoBest    : {autoBest}");
    sb.AppendLine($"Strict      : {strict}");
    sb.AppendLine($"Model strict: {model.Strict}");
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
        var current = ctx.Key.Equals(selectedContextKey, StringComparison.OrdinalIgnoreCase);
        var marker = current && step == WizardStep.Context ? ">>" : current ? " *" : "  ";
        sb.AppendLine($"{marker} {ctx.Label,-8} {ctx.Tokens,7} tokens{note}");
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
            var marker = q.Key == selectedQuant && step == WizardStep.Quant ? ">>" : q.Key == selectedQuant ? " *" : "  ";
            sb.AppendLine($"{marker} {q.Key,-16} {size,8} {Empty(q.Fit),-6}{current}{selected}");
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

static string FormatPicker(WizardStep step, List<PickerChoice> choices, int selectedIndex)
{
    var sb = new StringBuilder();
    sb.AppendLine($"Select {step}");
    sb.AppendLine();
    sb.AppendLine("Up/Down moves, Enter accepts, Left/Esc cancels.");
    sb.AppendLine();

    for (var i = 0; i < choices.Count; i++)
    {
        var marker = i == selectedIndex ? ">>" : "  ";
        sb.AppendLine($"{marker} {choices[i].Label}");
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

static bool IsKey(dynamic key, char expected)
{
    return TryPrintable(key, out var value) && char.ToLowerInvariant(value) == expected;
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

sealed record WizardRow(WizardStep Step, string Label, string Value)
{
    public override string ToString()
    {
        return $"{Label,-8} {Value}";
    }
}

sealed record PickerChoice(string Label, Action Apply);

enum ActivePane
{
    Models,
    Wizard,
    Choices
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
