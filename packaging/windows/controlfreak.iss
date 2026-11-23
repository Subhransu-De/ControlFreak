; Build through cargo xtask package. No signing or runtime downloads.
#if VER != 0x06070100
  #error Inno Setup 6.7.1 is required
#endif
#ifndef Version
  #error Version is required
#endif
#ifndef PayloadDir
  #error PayloadDir is required
#endif
#ifndef OutputPath
  #error OutputPath is required
#endif
#ifndef NumericVersion
  #error NumericVersion is required
#endif
#ifdef TestSetup
  #define Identity "ControlFreak.Installer.Test"
#else
  #define Identity "ControlFreak.Windows.x64"
#endif

[Setup]
AppId={#Identity}
AppName=ControlFreak
AppVersion={#Version}
#ifdef TestSetup
VersionInfoDescription=ControlFreak installer test
#else
VersionInfoDescription=ControlFreak setup
#endif
AppPublisher=ControlFreak contributors
AppPublisherURL=https://github.com/Subhransu-De/ControlFreak
DefaultDirName={localappdata}\Programs\ControlFreak
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
DisableWelcomePage=yes
DisableDirPage=no
DisableProgramGroupPage=yes
DisableReadyPage=yes
WizardStyle=modern
WizardSizePercent=120,130
SetupIconFile=controlfreak.ico
WizardImageFile=wizard-sidebar.png
WizardSmallImageFile=wizard-small.png
SetupMutex={#Identity}.Setup
CloseApplications=no
RestartApplications=no
UninstallDisplayName=ControlFreak
UninstallDisplayIcon={uninstallexe}
OutputDir={#OutputPath}
OutputBaseFilename=ControlFreak-{#Version}
VersionInfoVersion={#NumericVersion}
Compression=lzma2
SolidCompression=yes

[Files]
Source: "{#PayloadDir}\controlfreak.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\controlfreak-installer.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\controlfreak-installer.exe"; Flags: dontcopy
Source: "{#PayloadDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\SECURITY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\PRIVACY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\Cargo.lock"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\version.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\sbom\*"; DestDir: "{app}\sbom"; Flags: ignoreversion recursesubdirs createallsubdirs

[UninstallDelete]
Type: files; Name: "{app}\setup-results.txt"
Type: files; Name: "{app}\installer-state\claude-code.managed"
Type: files; Name: "{app}\installer-state\codex.managed"
Type: dirifempty; Name: "{app}\installer-state"

[Code]
const
  EM_SETSEL = $00B1;
  WM_COPY = $0301;

var
  ClientsPage: TWizardPage;
  ClientIds, ClientNames: TArrayOfString;
  ClientChecks: array[0..1] of TNewCheckBox;
  ClientCmdMemos: array[0..1] of TNewMemo;
  ClientCopyButtons: array[0..1] of TNewButton;
  ClientsHint: TNewStaticText;
  Details: TNewMemo;
  FinishClientLabels: array[0..1] of TNewStaticText;
  FinishCmdMemos: array[0..1] of TNewMemo;
  FinishCopyButtons: array[0..1] of TNewButton;
  ClientFailedFlags: array[0..1] of Boolean;
  SetupResults: String;
  AnySelected: Boolean;
  ConfigurationFailed: Boolean;
  FilesBlocked: Boolean;
  BlockerDetails: TNewMemo;
  RetryButton: TNewButton;
  BlockerText: String;
  BlockerCount: Integer;

function Quote(Value: String): String;
begin
  Result := '"' + Value + '"';
end;

function PowerShellQuote(Value: String): String;
var
  Escaped: String;
begin
  // PowerShell treats ASCII and Unicode curly single quotes as single-quote string delimiters;
  // doubling each keeps the path strictly literal.
  Escaped := Value;
  StringChangeEx(Escaped, '''', '''''', True);
  StringChangeEx(Escaped, #$2018, #$2018 + #$2018, True);
  StringChangeEx(Escaped, #$2019, #$2019 + #$2019, True);
  StringChangeEx(Escaped, #$201A, #$201A + #$201A, True);
  StringChangeEx(Escaped, #$201B, #$201B + #$201B, True);
  Result := '''' + Escaped + '''';
end;

function HelperPath: String;
begin
  Result := ExpandConstant('{tmp}\controlfreak-installer.exe');
end;

function InvokeHelper(Arguments: String; var ExitCode: Integer): Boolean;
begin
  Result := Exec(HelperPath, Arguments, '', SW_HIDE, ewWaitUntilTerminated, ExitCode);
end;

function ClientCommandForDir(Directory: String; Index: Integer): String;
var
  ExePath: String;
begin
  ExePath := PowerShellQuote(AddBackslash(Directory) + 'controlfreak.exe');
  if Index = 0 then
    Result := 'claude mcp add --scope user controlfreak -- ' + ExePath
  else
    Result := 'codex mcp add controlfreak -- ' + ExePath;
end;

function ClientCommand(Index: Integer): String;
begin
  Result := ClientCommandForDir(WizardForm.DirEdit.Text, Index);
end;

function CopyMemoToClipboard(Memo: TNewMemo): Boolean;
begin
  SendMessage(Memo.Handle, EM_SETSEL, 0, -1);
  SendMessage(Memo.Handle, WM_COPY, 0, 0);
  SendMessage(Memo.Handle, EM_SETSEL, 0, 0);
  Result := True;
end;

procedure UpdateClientsPagePreview;
var
  I: Integer;
  Cmd: String;
begin
  for I := 0 to 1 do begin
    Cmd := ClientCommand(I);
    ClientCmdMemos[I].Text := Cmd;
    if Length(Cmd) > 195 then
      ClientCmdMemos[I].ScrollBars := ssVertical
    else
      ClientCmdMemos[I].ScrollBars := ssNone;
    ClientCopyButtons[I].Caption := 'Copy';
  end;

  if ClientChecks[0].Checked and ClientChecks[1].Checked then
    ClientsHint.Caption := 'Setup will run both checked commands after installing files.'
  else if ClientChecks[0].Checked then
    ClientsHint.Caption := 'Setup will run the Claude Code command after installing files.'
  else if ClientChecks[1].Checked then
    ClientsHint.Caption := 'Setup will run the Codex command after installing files.'
  else
    ClientsHint.Caption := 'Optional: leave unchecked to install ControlFreak files only.';

  if WizardForm.CurPageID = ClientsPage.ID then
    WizardForm.NextButton.Caption := 'Install';
end;

procedure ClientCheckChanged(Sender: TObject);
begin
  UpdateClientsPagePreview;
end;

procedure CopyClientCommand(Sender: TObject);
var
  Idx: Integer;
begin
  Idx := TNewButton(Sender).Tag;
  CopyMemoToClipboard(ClientCmdMemos[Idx]);
  ClientCopyButtons[Idx].Caption := 'Copied!';
  ClientCopyButtons[1 - Idx].Caption := 'Copy';
end;

procedure CopyFinishClientCommand(Sender: TObject);
var
  Idx: Integer;
begin
  Idx := TNewButton(Sender).Tag;
  CopyMemoToClipboard(FinishCmdMemos[Idx]);
  FinishCopyButtons[Idx].Caption := 'Copied!';
  FinishCopyButtons[1 - Idx].Caption := 'Copy';
end;

function FileAvailable(Path: String): Boolean;
var
  Stream: TFileStream;
begin
  Result := True;
  if not FileExists(Path) then exit;
  try
    Stream := TFileStream.Create(Path, fmOpenReadWrite or fmShareExclusive);
    Stream.Free;
  except
    Result := False;
  end;
end;

function InstalledFilesAvailable(Directory: String): Boolean;
begin
  Result := FileAvailable(AddBackslash(Directory) + 'controlfreak.exe') and
    FileAvailable(AddBackslash(Directory) + 'controlfreak-installer.exe');
end;

function BlockingMessage(Directory, Helper, Mode: String): String;
var
  Report: String;
  Code, I, Count: Integer;
begin
  Report := ExpandConstant('{tmp}\controlfreak-blockers.ini');
  DeleteFile(Report);
  Exec(Helper, 'blockers ' + Quote(Directory) + ' ' + Quote(Report) + ' ' + Mode, '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := GetIniString('result', 'message',
    'Setup cannot replace the ControlFreak files.', Report) + #13#10 + #13#10 +
    GetIniString('result', 'action',
      'Close clients using ControlFreak and check that you can write to the installation folder. Blocking processes could not be identified.', Report);
  Count := GetIniInt('result', 'count', 0, 0, 4096, Report);
  BlockerCount := Count;
  if Count > 0 then begin
    Result := Result + #13#10 + #13#10 + 'Processes to stop:';
    for I := 0 to Count - 1 do
      Result := Result + #13#10 + '  ' + GetIniString('result', 'process' + IntToStr(I), '', Report);
  end;
end;

procedure RetryInstall(Sender: TObject);
begin
  RetryButton.Enabled := False;
  // Re-enter preparation through normal wizard navigation so every check runs
  // again. Advancing directly from a failed wpPreparing would exit Setup.
  WizardForm.BackButton.OnClick(WizardForm.BackButton);
  WizardForm.NextButton.OnClick(WizardForm.NextButton);
end;

procedure InitializeWizard;
var
  I, Code, Y: Integer;
  Selection, ProbeReport, ClientStatus, LabelText: String;
  Selected: TArrayOfString;
  ClientsIntro: TNewStaticText;
begin
  ClientIds := ['claude-code', 'codex'];
  ClientNames := ['Claude Code', 'Codex'];
  ExtractTemporaryFile('controlfreak-installer.exe');

  BlockerDetails := TNewMemo.Create(WizardForm);
  BlockerDetails.Parent := WizardForm.PreparingPage;
  BlockerDetails.SetBounds(WizardForm.PreparingLabel.Left, WizardForm.PreparingLabel.Top,
    WizardForm.PreparingLabel.Width, WizardForm.PreparingPage.Height - WizardForm.PreparingLabel.Top - ScaleY(8));
  BlockerDetails.ReadOnly := True;
  BlockerDetails.BorderStyle := bsNone;
  BlockerDetails.Color := WizardForm.PreparingPage.Color;
  BlockerDetails.ScrollBars := ssNone;
  BlockerDetails.WordWrap := True;
  BlockerDetails.Visible := False;

  RetryButton := TNewButton.Create(WizardForm);
  RetryButton.Parent := WizardForm.NextButton.Parent;
  RetryButton.SetBounds(WizardForm.NextButton.Left, WizardForm.NextButton.Top,
    WizardForm.NextButton.Width, WizardForm.NextButton.Height);
  RetryButton.Caption := 'Try Again';
  RetryButton.OnClick := @RetryInstall;
  RetryButton.Visible := False;

  ClientsPage := CreateCustomPage(wpSelectDir, 'Connect MCP clients (optional)',
    'Let Setup run a client command for you, or copy each one to run in PowerShell.');

  ClientsIntro := TNewStaticText.Create(ClientsPage);
  ClientsIntro.Parent := ClientsPage.Surface;
  ClientsIntro.Left := 0;
  ClientsIntro.Top := 0;
  ClientsIntro.Width := ClientsPage.SurfaceWidth;
  ClientsIntro.WordWrap := True;
  ClientsIntro.AutoSize := False;
  ClientsIntro.Caption := 'Check a client to register it after installing files, or copy a PowerShell command to run yourself.';
  ClientsIntro.AdjustHeight;

  ProbeReport := '';
  if not WizardSilent then begin
    ProbeReport := ExpandConstant('{tmp}\controlfreak-probe.ini');
    DeleteFile(ProbeReport);
    if not InvokeHelper('probe-clients ' + Quote(ProbeReport), Code) then
      ProbeReport := '';
  end;

  Y := ClientsIntro.Top + ClientsIntro.Height + ScaleY(14);
  for I := 0 to 1 do begin
    LabelText := ClientNames[I];
    if ProbeReport <> '' then begin
      ClientStatus := GetIniString('clients', ClientIds[I], '', ProbeReport);
      if ClientStatus <> '' then
        LabelText := LabelText + ' (' + ClientStatus + ')';
    end;
    ClientChecks[I] := TNewCheckBox.Create(ClientsPage);
    ClientChecks[I].Parent := ClientsPage.Surface;
    ClientChecks[I].Left := 0;
    ClientChecks[I].Top := Y + ScaleY(2);
    ClientChecks[I].Width := ClientsPage.SurfaceWidth - ScaleX(88);
    ClientChecks[I].Height := ScaleY(20);
    ClientChecks[I].Caption := LabelText;
    ClientChecks[I].OnClick := @ClientCheckChanged;
    if (ProbeReport <> '') and (GetIniInt('clients', 'elevated', 0, 0, 1, ProbeReport) = 1) then
      ClientChecks[I].Enabled := False;

    ClientCopyButtons[I] := TNewButton.Create(ClientsPage);
    ClientCopyButtons[I].Parent := ClientsPage.Surface;
    ClientCopyButtons[I].Left := ClientsPage.SurfaceWidth - ScaleX(80);
    ClientCopyButtons[I].Top := Y;
    ClientCopyButtons[I].Width := ScaleX(80);
    ClientCopyButtons[I].Height := ScaleY(24);
    ClientCopyButtons[I].Caption := 'Copy';
    ClientCopyButtons[I].Tag := I;
    ClientCopyButtons[I].OnClick := @CopyClientCommand;

    ClientCmdMemos[I] := TNewMemo.Create(ClientsPage);
    ClientCmdMemos[I].Parent := ClientsPage.Surface;
    ClientCmdMemos[I].Left := 0;
    ClientCmdMemos[I].Top := Y + ScaleY(28);
    ClientCmdMemos[I].Width := ClientsPage.SurfaceWidth;
    ClientCmdMemos[I].Height := ScaleY(48);
    ClientCmdMemos[I].ReadOnly := True;
    ClientCmdMemos[I].WordWrap := True;
    ClientCmdMemos[I].ScrollBars := ssNone;
    ClientCmdMemos[I].Font.Name := 'Consolas';
    ClientCmdMemos[I].Font.Size := 8;

    Y := ClientCmdMemos[I].Top + ClientCmdMemos[I].Height + ScaleY(14);
  end;

  ClientsHint := TNewStaticText.Create(ClientsPage);
  ClientsHint.Parent := ClientsPage.Surface;
  ClientsHint.Left := 0;
  ClientsHint.Top := Y;
  ClientsHint.Width := ClientsPage.SurfaceWidth;
  ClientsHint.WordWrap := True;
  ClientsHint.AutoSize := False;
  ClientsHint.Caption := 'Optional: leave unchecked to install ControlFreak files only.';
  ClientsHint.AdjustHeight;

  Selection := ExpandConstant('{param:CLIENTS|}');
  if Selection <> '' then begin
    Selected := StringSplit(Selection, [','], stExcludeEmpty);
    for I := 0 to GetArrayLength(Selected) - 1 do
      if (Selected[I] <> 'claude-code') and (Selected[I] <> 'codex') then
        RaiseException('Unknown /CLIENTS value.');
    for I := 0 to 1 do
      ClientChecks[I].Checked := Pos(',' + ClientIds[I] + ',', ',' + Selection + ',') > 0;
  end;
  UpdateClientsPagePreview;
end;

procedure CurPageChanged(CurPageID: Integer);
var
  FinishedNote, DisplayPath, Cmd: String;
  I, ResultsHeight, CmdTop, BlockHeight, ShowCount: Integer;
  ShowClient: Boolean;
begin
  RetryButton.Visible := (CurPageID = wpPreparing) and FilesBlocked;
  BlockerDetails.Visible := RetryButton.Visible;
  if RetryButton.Visible then begin
    WizardForm.PreparingLabel.Visible := False;
    if BlockerCount > 10 then
      BlockerDetails.ScrollBars := ssVertical
    else
      BlockerDetails.ScrollBars := ssNone;
    BlockerDetails.Text := BlockerText;
    WizardForm.NextButton.Visible := False;
    RetryButton.Enabled := True;
    RetryButton.Default := True;
    if not WizardSilent then WizardForm.ActiveControl := RetryButton;
  end else RetryButton.Default := False;

  if CurPageID = ClientsPage.ID then begin
    UpdateClientsPagePreview;
    WizardForm.NextButton.Caption := 'Install';
  end;

  if CurPageID = wpFinished then begin
    if ConfigurationFailed then
      WizardForm.FinishedHeadingLabel.Caption := 'ControlFreak installed with warnings';
    DisplayPath := MinimizePathName(ExpandConstant('{app}'), WizardForm.FinishedLabel.Font,
      WizardForm.FinishedHeadingLabel.Width - ScaleX(80));
    if ConfigurationFailed then
      FinishedNote := 'Installed to: ' + DisplayPath + #13#10 + #13#10 +
        'ControlFreak files installed, but a CLI command failed. Copy and run it in PowerShell:'
    else if AnySelected then
      FinishedNote := 'Installed to: ' + DisplayPath + #13#10 + #13#10 +
        'Restart configured clients to load ControlFreak.'
    else
      FinishedNote := 'Installed to: ' + DisplayPath + #13#10 + #13#10 +
        'ControlFreak is installed. To connect a client later, copy a PowerShell command below:';

    WizardForm.FinishedLabel.WordWrap := True;
    WizardForm.FinishedLabel.AutoSize := False;
    WizardForm.FinishedLabel.Width := WizardForm.FinishedHeadingLabel.Width;
    WizardForm.FinishedLabel.Height := ScaleY(1);
    WizardForm.FinishedLabel.Caption := FinishedNote;
    WizardForm.FinishedLabel.AdjustHeight;
    WizardForm.FinishedLabel.Height := WizardForm.FinishedLabel.Height + ScaleY(4);

    if Details = nil then begin
      Details := TNewMemo.Create(WizardForm);
      Details.Parent := WizardForm.FinishedPage;
      Details.ReadOnly := True;
      Details.BorderStyle := bsNone;
      Details.Color := WizardForm.FinishedPage.Color;
      Details.WordWrap := True;
      Details.ScrollBars := ssNone;
    end;
    for I := 0 to 1 do begin
      if FinishClientLabels[I] = nil then begin
        FinishClientLabels[I] := TNewStaticText.Create(WizardForm);
        FinishClientLabels[I].Parent := WizardForm.FinishedPage;
        FinishClientLabels[I].Font.Style := [fsBold];
      end;
      if FinishCopyButtons[I] = nil then begin
        FinishCopyButtons[I] := TNewButton.Create(WizardForm);
        FinishCopyButtons[I].Parent := WizardForm.FinishedPage;
        FinishCopyButtons[I].Width := ScaleX(76);
        FinishCopyButtons[I].Height := ScaleY(22);
        FinishCopyButtons[I].Tag := I;
        FinishCopyButtons[I].OnClick := @CopyFinishClientCommand;
      end;
      if FinishCmdMemos[I] = nil then begin
        FinishCmdMemos[I] := TNewMemo.Create(WizardForm);
        FinishCmdMemos[I].Parent := WizardForm.FinishedPage;
        FinishCmdMemos[I].ReadOnly := True;
        FinishCmdMemos[I].WordWrap := True;
        FinishCmdMemos[I].ScrollBars := ssNone;
        FinishCmdMemos[I].Font.Name := 'Consolas';
        FinishCmdMemos[I].Font.Size := 8;
      end;
    end;

    ShowCount := 0;
    for I := 0 to 1 do begin
      if (not ClientChecks[I].Checked) or ClientFailedFlags[I] then
        ShowCount := ShowCount + 1;
    end;

    CmdTop := WizardForm.FinishedLabel.Top + WizardForm.FinishedLabel.Height + ScaleY(8);
    if AnySelected then begin
      Details.Visible := True;
      Details.Left := WizardForm.FinishedLabel.Left;
      Details.Width := WizardForm.FinishedLabel.Width;
      Details.Top := CmdTop;
      if ShowCount = 2 then
        ResultsHeight := ScaleY(38)
      else if ShowCount = 1 then
        ResultsHeight := ScaleY(42)
      else
        ResultsHeight := WizardForm.FinishedPage.Height - Details.Top - ScaleY(12);
      Details.Height := ResultsHeight;
      if (ShowCount > 0) and (Length(SetupResults) > 120) then
        Details.ScrollBars := ssVertical
      else
        Details.ScrollBars := ssNone;
      Details.Text := SetupResults;
      CmdTop := Details.Top + Details.Height + ScaleY(6);
    end else begin
      Details.Visible := False;
    end;

    if (ShowCount = 2) and AnySelected then
      BlockHeight := ScaleY(42)
    else
      BlockHeight := ScaleY(50);

    for I := 0 to 1 do begin
      ShowClient := (not ClientChecks[I].Checked) or ClientFailedFlags[I];
      FinishClientLabels[I].Visible := ShowClient;
      FinishCopyButtons[I].Visible := ShowClient;
      FinishCmdMemos[I].Visible := ShowClient;
      if ShowClient then begin
        if (not ClientChecks[I].Checked) and AnySelected then
          FinishClientLabels[I].Caption := ClientNames[I] + ' (optional, PowerShell)'
        else
          FinishClientLabels[I].Caption := ClientNames[I] + ' (PowerShell)';
        FinishClientLabels[I].Left := WizardForm.FinishedLabel.Left;
        FinishClientLabels[I].Top := CmdTop + ScaleY(3);

        FinishCopyButtons[I].Caption := 'Copy';
        FinishCopyButtons[I].Left := WizardForm.FinishedLabel.Left + WizardForm.FinishedLabel.Width - FinishCopyButtons[I].Width;
        FinishCopyButtons[I].Top := CmdTop;

        Cmd := ClientCommandForDir(ExpandConstant('{app}'), I);
        FinishCmdMemos[I].Left := WizardForm.FinishedLabel.Left;
        FinishCmdMemos[I].Top := CmdTop + ScaleY(24);
        FinishCmdMemos[I].Width := WizardForm.FinishedLabel.Width;
        FinishCmdMemos[I].Height := BlockHeight;
        if Length(Cmd) > 180 then
          FinishCmdMemos[I].ScrollBars := ssVertical
        else
          FinishCmdMemos[I].ScrollBars := ssNone;
        FinishCmdMemos[I].Text := Cmd;

        CmdTop := FinishCmdMemos[I].Top + FinishCmdMemos[I].Height + ScaleY(8);
      end;
    end;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  PreviousDirectory, PreviousVersion, DisplayPrevDir: String;
  Code: Integer;
begin
  Result := '';
  FilesBlocked := False;
  if RegQueryStringValue(HKCU64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#Identity}_is1', 'InstallLocation', PreviousDirectory) then begin
    if CompareText(RemoveBackslashUnlessRoot(PreviousDirectory), RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) <> 0 then begin
      DisplayPrevDir := MinimizePathName(RemoveBackslashUnlessRoot(PreviousDirectory),
        WizardForm.PreparingLabel.Font, WizardForm.PreparingLabel.Width - ScaleX(12));
      Result := 'ControlFreak is already installed at:' + #13#10 +
        DisplayPrevDir + #13#10 + #13#10 +
        'Click Back and select that folder to upgrade, or uninstall ControlFreak before moving it.';
      exit;
    end;
    if not RegQueryStringValue(HKCU64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#Identity}_is1', 'DisplayVersion', PreviousVersion) then begin
      Result := 'Cannot determine the installed version. Uninstall it before continuing.';
      exit;
    end;
    if (not InvokeHelper('check-version ' + Quote(PreviousVersion), Code)) or (Code <> 0) then begin
      Result := 'ControlFreak ' + PreviousVersion + ' is already installed, which is newer than or incompatible with version {#Version}. Uninstall the existing version before installing this one.';
      exit;
    end;
  end;
  if not InstalledFilesAvailable(ExpandConstant('{app}')) then begin
    FilesBlocked := True;
    BlockerText := BlockingMessage(ExpandConstant('{app}'), HelperPath, 'install') + #13#10 + #13#10 +
      'When finished, click Try Again to continue installation.';
    // Keep process details out of Inno Setup's automatic failure log.
    Result := 'ControlFreak files cannot be updated.';
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  I, Code: Integer;
  Report, MessageText, Unselected: String;
  ClientFailed: Boolean;
begin
  if CurStep <> ssPostInstall then exit;
  SetupResults := '';
  Unselected := '';
  AnySelected := False;
  ConfigurationFailed := False;
  ClientFailedFlags[0] := False;
  ClientFailedFlags[1] := False;
  for I := 0 to 1 do begin
    if ClientChecks[I].Checked then begin
      AnySelected := True;
      WizardForm.StatusLabel.Caption := 'Registering ' + ClientNames[I] + '...';
      Report := ExpandConstant('{tmp}\register-') + ClientIds[I] + '.ini';
      DeleteFile(Report);
      if WizardSilent and (ExpandConstant('{param:ALLOWELEVATEDCLI|0}') = '1') then
        ClientFailed := (not InvokeHelper('register-cli --allow-elevated ' + ClientIds[I] + ' ' + Quote(ExpandConstant('{app}\controlfreak.exe')) + ' ' +
          Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report), Code)) or (Code <> 0)
      else
        ClientFailed := (not InvokeHelper('register-cli ' + ClientIds[I] + ' ' + Quote(ExpandConstant('{app}\controlfreak.exe')) + ' ' +
          Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report), Code)) or (Code <> 0);
      if ClientFailed then begin
        ConfigurationFailed := True;
        ClientFailedFlags[I] := True;
      end;
      MessageText := GetIniString('result', 'message', 'CLI registration failed; run the command manually.', Report);
      if SetupResults <> '' then SetupResults := SetupResults + #13#10;
      if ClientFailed then
        SetupResults := SetupResults + #$2022 + ' ' + ClientNames[I] + ': Failed - ' + MessageText
      else
        SetupResults := SetupResults + #$2022 + ' ' + ClientNames[I] + ': ' + MessageText;
    end else begin
      if Unselected <> '' then Unselected := Unselected + ', ';
      Unselected := Unselected + ClientNames[I];
    end;
  end;
  if not AnySelected then begin
    SetupResults := 'Skipped client registration.' + #13#10 + #13#10 +
      ClientCommandForDir(ExpandConstant('{app}'), 0) + #13#10 +
      ClientCommandForDir(ExpandConstant('{app}'), 1);
  end else if Unselected <> '' then begin
    SetupResults := SetupResults + #13#10 + #13#10 + 'Not selected: ' + Unselected;
  end;
  if not SaveStringToFile(ExpandConstant('{app}\setup-results.txt'),
    Utf8Encode('Installed to: ' + ExpandConstant('{app}') + #13#10 + #13#10 + SetupResults + #13#10), False) then
    Log('Could not save setup-results.txt.');
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if ConfigurationFailed then Result := 10;
end;

function InitializeUninstall: Boolean;
begin
  Result := InstalledFilesAvailable(ExpandConstant('{app}'));
  if not Result then
    SuppressibleMsgBox(BlockingMessage(ExpandConstant('{app}'), ExpandConstant('{app}\controlfreak-installer.exe'), 'uninstall') + #13#10 + #13#10 +
      'Run uninstall again after resolving the problem. No processes were stopped.', mbError, MB_OK, IDOK);
end;

function HasLegacyReceipts: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\installer-state\claude-code.json')) or
    FileExists(ExpandConstant('{app}\installer-state\codex.json')) or
    FileExists(ExpandConstant('{app}\installer-state\claude-desktop.json')) or
    FileExists(ExpandConstant('{app}\installer-state\pi.json')) or
    FileExists(ExpandConstant('{app}\installer-state\opencode.json'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  RemoveEntries, HasManaged, HasLegacy: Boolean;
  Report, MessageText, LegacyNote: String;
  Code: Integer;
begin
  if CurUninstallStep <> usUninstall then exit;
  HasManaged := FileExists(ExpandConstant('{app}\installer-state\claude-code.managed')) or
    FileExists(ExpandConstant('{app}\installer-state\codex.managed'));
  HasLegacy := HasLegacyReceipts;
  if (not HasManaged) and (not HasLegacy) then exit;

  LegacyNote := 'Earlier Setup versions configured MCP client files directly. Remove any remaining ControlFreak entries manually, and delete old *.controlfreak-backup-*.bak files (which may contain credentials) once no longer needed.';

  if not HasManaged then begin
    Log('Legacy MCP receipts detected: ' + LegacyNote);
    SuppressibleMsgBox(LegacyNote, mbInformation, MB_OK, IDOK);
    exit;
  end;

  RemoveEntries := ExpandConstant('{param:REMOVECONFIG|0}') = '1';
  if not UninstallSilent then
    RemoveEntries := SuppressibleMsgBox('Remove the ControlFreak MCP entries registered by Setup using the Claude Code / Codex CLI? (For Claude Code, decline if you customized the entry after install.)', mbConfirmation, MB_YESNO, IDNO) = IDYES;
  if RemoveEntries then begin
    Report := ExpandConstant('{tmp}\controlfreak-remove.ini');
    DeleteFile(Report);
    if UninstallSilent and (ExpandConstant('{param:ALLOWELEVATEDCLI|0}') = '1') then
      MessageText := 'remove-cli --allow-elevated '
    else
      MessageText := 'remove-cli ';
    if not Exec(ExpandConstant('{app}\controlfreak-installer.exe'), MessageText + Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report), '', SW_HIDE, ewWaitUntilTerminated, Code) then begin
      MessageText := 'The setup helper is unavailable. Uninstall will continue; remove ControlFreak manually with claude mcp remove / codex mcp remove.';
      if HasLegacy then
        MessageText := MessageText + #13#10 + #13#10 + LegacyNote;
      Log(MessageText);
      SuppressibleMsgBox(MessageText, mbInformation, MB_OK, IDOK);
      exit;
    end;
    if (Code <> 0) or (GetIniString('result', 'status', 'error', Report) <> 'ok') then begin
      MessageText := GetIniString('result', 'message', 'MCP cleanup could not be verified. Remove ControlFreak manually with claude mcp remove / codex mcp remove.', Report);
      Log('Optional MCP cleanup: ' + MessageText);
      SuppressibleMsgBox('Application removal will continue.' + #13#10 + #13#10 + MessageText, mbInformation, MB_OK, IDOK);
    end;
  end else if HasLegacy then begin
    Log('Legacy MCP receipts detected: ' + LegacyNote);
    SuppressibleMsgBox(LegacyNote, mbInformation, MB_OK, IDOK);
  end;
end;
