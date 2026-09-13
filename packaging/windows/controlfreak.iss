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
OutputBaseFilename=ControlFreak-{#Version}-Setup
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

procedure InitializeWizard;
var
  I: Integer;
  Selection: String;
  Selected: TArrayOfString;
begin
  ClientIds := ['codex', 'claude-code', 'claude-desktop', 'pi', 'opencode'];
  ClientNames := ['Codex', 'Claude Code', 'Claude Desktop', 'Pi', 'OpenCode'];
  ExtractTemporaryFile('controlfreak-installer.exe');
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
  if not InstalledFilesAvailable(ExpandConstant('{app}')) then
    Result := 'ControlFreak files are in use or not writable. Stop the MCP clients using this installation, then click Retry. Setup never stops them automatically.';
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
    SuppressibleMsgBox('Stop the MCP clients using ControlFreak and retry uninstall. No processes were stopped.', mbError, MB_OK, IDOK);
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
    if (not Exec(ExpandConstant('{app}\controlfreak-installer.exe'), 'remove ' + Quote(ExpandConstant('{app}\installer-state')) + ' ' + Quote(Report), '', SW_HIDE, ewWaitUntilTerminated, Code)) or (Code <> 0) then begin
      MessageText := GetIniString('result', 'message', 'MCP configuration cleanup failed. Use the README manual configuration instructions.', Report);
      SuppressibleMsgBox(MessageText, mbError, MB_OK, IDOK);
      RaiseException('Configuration cleanup failed; fix the configuration and retry uninstall.');
    end;
  end;
end;
