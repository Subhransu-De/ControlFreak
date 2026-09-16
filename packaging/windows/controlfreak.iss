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
SetupMutex={#Identity}.Setup
CloseApplications=no
RestartApplications=no
UninstallDisplayIcon={app}\controlfreak.exe
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
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\Cargo.lock"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\version.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\sbom\*"; DestDir: "{app}\sbom"; Flags: ignoreversion recursesubdirs createallsubdirs

[UninstallDelete]
Type: files; Name: "{app}\setup-results.txt"
Type: dirifempty; Name: "{app}\installer-state"

[Code]
var
  ClientsPage: TInputOptionWizardPage;
  Details: TNewMemo;
  ClientIds, ClientNames: TArrayOfString;
  SetupResults: String;
  ConfigurationFailed: Boolean;
  FilesBlocked: Boolean;
  BlockerDetails: TNewMemo;
  RetryButton: TNewButton;
  BlockerText: String;

function Quote(Value: String): String;
begin
  Result := '"' + Value + '"';
end;

function HelperPath: String;
begin
  Result := ExpandConstant('{tmp}\controlfreak-installer.exe');
end;

function InvokeHelper(Arguments: String; var ExitCode: Integer): Boolean;
begin
  Result := Exec(HelperPath, Arguments, '', SW_HIDE, ewWaitUntilTerminated, ExitCode);
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

function BlockingMessage(Directory, Helper: String): String;
var
  Report: String;
  Code, I, Count: Integer;
begin
  Report := ExpandConstant('{tmp}\controlfreak-blockers.ini');
  DeleteFile(Report);
  Exec(Helper, 'blockers ' + Quote(Directory) + ' ' + Quote(Report), '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := GetIniString('result', 'message',
    'Setup cannot replace the ControlFreak files.', Report) + #13#10 + #13#10 +
    GetIniString('result', 'action',
      'Close clients using ControlFreak and check that you can write to the installation folder. Blocking processes could not be identified.', Report);
  Count := GetIniInt('result', 'count', 0, 0, 4096, Report);
  if Count > 0 then begin
    Result := Result + #13#10 + #13#10 + 'Processes to stop:';
    for I := 0 to Count - 1 do
      Result := Result + #13#10 + '  ' + #$2022 + ' ' + GetIniString('result', 'process' + IntToStr(I), '', Report);
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
  I: Integer;
  Selection: String;
  Selected: TArrayOfString;
begin
  ClientIds := ['codex', 'claude-code', 'claude-desktop', 'pi', 'opencode'];
  ClientNames := ['Codex', 'Claude Code', 'Claude Desktop', 'Pi', 'OpenCode'];
  ExtractTemporaryFile('controlfreak-installer.exe');
  BlockerDetails := TNewMemo.Create(WizardForm);
  BlockerDetails.Parent := WizardForm.PreparingPage;
  BlockerDetails.SetBounds(WizardForm.PreparingLabel.Left, WizardForm.PreparingLabel.Top,
    WizardForm.PreparingLabel.Width, WizardForm.PreparingPage.Height - WizardForm.PreparingLabel.Top);
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
  ClientsPage := CreateInputOptionPage(wpSelectDir, 'Configure MCP clients',
    'Choose which existing clients can use ControlFreak.',
    'Select clients to install or update their ControlFreak connection. Close selected clients before installing.', False, False);
  ClientsPage.CheckListBox.Height := ScaleY(125);
  for I := 0 to 4 do ClientsPage.Add(ClientNames[I]);
  Selection := ExpandConstant('{param:CLIENTS|}');
  if Selection <> '' then begin
    Selected := StringSplit(Selection, [','], stExcludeEmpty);
    for I := 0 to GetArrayLength(Selected) - 1 do
      if (Selected[I] <> 'codex') and (Selected[I] <> 'claude-code') and
        (Selected[I] <> 'claude-desktop') and (Selected[I] <> 'pi') and
        (Selected[I] <> 'opencode') then RaiseException('Unknown /CLIENTS value.');
    for I := 0 to 4 do
      ClientsPage.Values[I] := Pos(',' + ClientIds[I] + ',', ',' + Selection + ',') > 0;
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  RetryButton.Visible := (CurPageID = wpPreparing) and FilesBlocked;
  BlockerDetails.Visible := RetryButton.Visible;
  if RetryButton.Visible then begin
    WizardForm.PreparingLabel.Visible := False;
    BlockerDetails.Text := BlockerText;
    WizardForm.NextButton.Visible := False;
    RetryButton.Enabled := True;
    RetryButton.Default := True;
    if not WizardSilent then WizardForm.ActiveControl := RetryButton;
  end else RetryButton.Default := False;
  if CurPageID = ClientsPage.ID then
    WizardForm.NextButton.Caption := 'Install';
  if CurPageID = wpFinished then begin
    WizardForm.FinishedLabel.Caption := 'ControlFreak is installed at ' + ExpandConstant('{app}') + '.' + #13#10 +
      'Quit and reopen configured clients. Pi requires pi-mcp-adapter.' + #13#10 +
      'Configuration results are saved in setup-results.txt in the installation directory.';
    if Details = nil then begin
      Details := TNewMemo.Create(WizardForm);
      Details.Parent := WizardForm.FinishedPage;
      Details.Width := ClientsPage.SurfaceWidth;
      Details.ReadOnly := True;
      Details.ScrollBars := ssVertical;
      Details.WordWrap := True;
    end;
    Details.Top := WizardForm.FinishedLabel.Top + WizardForm.FinishedLabel.Height + ScaleY(12);
    Details.Height := WizardForm.FinishedPage.Height - Details.Top;
    Details.Text := SetupResults;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  PreviousDirectory, PreviousVersion: String;
  Code: Integer;
begin
  Result := '';
  FilesBlocked := False;
  if RegQueryStringValue(HKCU64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#Identity}_is1', 'InstallLocation', PreviousDirectory) then begin
    if CompareText(RemoveBackslashUnlessRoot(PreviousDirectory), RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) <> 0 then begin
      Result := 'Use the existing installation directory when upgrading, or uninstall ControlFreak before moving it.';
      exit;
    end;
    if not RegQueryStringValue(HKCU64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#Identity}_is1', 'DisplayVersion', PreviousVersion) then begin
      Result := 'Cannot determine the installed version. Uninstall it before continuing.';
      exit;
    end;
    if (not InvokeHelper('check-version ' + Quote(PreviousVersion), Code)) or (Code <> 0) then begin
      Result := 'A newer or unrecognized ControlFreak version is installed. Uninstall it before installing this version.';
      exit;
    end;
  end;
  if not InstalledFilesAvailable(ExpandConstant('{app}')) then begin
    FilesBlocked := True;
    BlockerText := BlockingMessage(ExpandConstant('{app}'), HelperPath) + #13#10 + #13#10 +
      'When finished, click Try Again to continue installation.';
    // Keep process details out of Inno Setup's automatic failure log.
    Result := 'ControlFreak files cannot be updated.';
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  I, Code: Integer;
  Report, MessageText: String;
begin
  if CurStep <> ssPostInstall then exit;
  SetupResults := '';
  for I := 0 to 4 do begin
    if ClientsPage.Values[I] then begin
      WizardForm.StatusLabel.Caption := 'Configuring ' + ClientNames[I] + '...';
      Report := ExpandConstant('{tmp}\configure-') + ClientIds[I] + '.ini';
      DeleteFile(Report);
      if (not InvokeHelper('configure ' + ClientIds[I] + ' ' + Quote(ExpandConstant('{app}\controlfreak.exe')) + ' ' +
        Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report) + ' replace', Code)) or (Code <> 0) then
        ConfigurationFailed := True;
      MessageText := GetIniString('result', 'message', 'Configuration failed. Re-run setup or use the manual README instructions.', Report);
    end else MessageText := 'Not selected; configuration unchanged.';
    SetupResults := SetupResults + ClientNames[I] + ': ' + MessageText + #13#10 + #13#10;
  end;
  if ConfigurationFailed then SetupResults := 'Installation succeeded, but some client configuration failed. Re-run setup after fixing the reported problem.' + #13#10 + #13#10 + SetupResults;
  SaveStringToFile(ExpandConstant('{app}\setup-results.txt'), SetupResults, False);
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
    SuppressibleMsgBox(BlockingMessage(ExpandConstant('{app}'), ExpandConstant('{app}\controlfreak-installer.exe')) + #13#10 +
      'Run uninstall again after resolving the problem. No processes were stopped.', mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  RemoveEntries: Boolean;
  Report, MessageText: String;
  Code: Integer;
begin
  if CurUninstallStep <> usUninstall then exit;
  RemoveEntries := ExpandConstant('{param:REMOVECONFIG|0}') = '1';
  if not UninstallSilent then
    RemoveEntries := SuppressibleMsgBox('Remove the MCP entries written by this installer? Entries edited since installation and configuration backups will be kept.', mbConfirmation, MB_YESNO, IDNO) = IDYES;
  if RemoveEntries then begin
    Report := ExpandConstant('{tmp}\controlfreak-remove.ini');
    DeleteFile(Report);
    if not Exec(ExpandConstant('{app}\controlfreak-installer.exe'), 'remove ' + Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report), '', SW_HIDE, ewWaitUntilTerminated, Code) then begin
      MessageText := 'The configuration helper is unavailable. Uninstall will continue and keep client configurations. Remove their ControlFreak entries manually.';
      Log(MessageText);
      SuppressibleMsgBox(MessageText, mbInformation, MB_OK, IDOK);
      exit;
    end;
    // Optional cleanup never vetoes application removal, including helper or
    // report failures. Show retained-entry and recovery warnings even on exit 0.
    if (Code <> 0) or (GetIniString('result', 'status', 'error', Report) <> 'ok') then begin
      MessageText := GetIniString('result', 'message', 'MCP configuration cleanup could not be verified. Check client configurations and their backups; remove remaining ControlFreak entries manually.', Report);
      Log('Optional MCP configuration cleanup: ' + MessageText);
      SuppressibleMsgBox('Application removal will continue.' + #13#10 + #13#10 + MessageText, mbInformation, MB_OK, IDOK);
    end;
  end;
end;
