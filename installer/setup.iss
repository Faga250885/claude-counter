#define MyAppName "Claude Counter"
#define MyAppVersion "1.4.8"
#define MyAppPublisher "Code Zeno Pty Ltd"
#define MyAppExeName "claude-code-usage-monitor.exe"

[Setup]
AppId={{BD7BBADB-0DE0-4B22-9502-2814000A44D4}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL=https://codezeno.com.au
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=output
OutputBaseFilename=ClaudeCounter-Setup
SetupIconFile=..\src\icons\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Crear un icono en el escritorio"; GroupDescription: "Iconos adicionales:"

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Iniciar {#MyAppName}"; Flags: nowait postinstall skipifsilent

[Code]
var
  ClaudeSetupPage: TWizardPage;
  ClaudeSetupMemo: TNewMemo;
  ClaudeSetupCheckBox: TNewCheckBox;

procedure InitializeWizard;
var
  InfoText: String;
begin
  ClaudeSetupPage := CreateCustomPage(wpInstalling, '', '');

  if ActiveLanguage = 'spanish' then
  begin
    ClaudeSetupPage.Caption := 'Configura Claude Code';
    ClaudeSetupPage.Description := 'Antes de continuar, confirma que completaste este paso';
    InfoText :=
      'Antes de usar Claude Counter, necesitas tener la CLI de Claude Code instalada e iniciada sesion en esta PC.' + #13#10 + #13#10 +
      'Si ya usas Claude Code en esta maquina, no necesitas hacer nada mas - la app detecta tus credenciales automaticamente.' + #13#10 + #13#10 +
      'Si es la primera vez, sigue estos pasos:' + #13#10 + #13#10 +
      '1. Abre PowerShell.' + #13#10 +
      '2. Ejecuta: irm https://claude.ai/install.ps1 | iex' + #13#10 +
      '3. Ejecuta: claude' + #13#10 +
      '4. Sigue las indicaciones para iniciar sesion (usa /login si no te lo pide automaticamente).';
  end
  else
  begin
    ClaudeSetupPage.Caption := 'Set up Claude Code';
    ClaudeSetupPage.Description := 'Before continuing, confirm you''ve completed this step';
    InfoText :=
      'Before using Claude Counter, you need the Claude Code CLI installed and signed in on this PC.' + #13#10 + #13#10 +
      'If you already use Claude Code on this machine, you don''t need to do anything else - the app detects your credentials automatically.' + #13#10 + #13#10 +
      'If this is your first time, follow these steps:' + #13#10 + #13#10 +
      '1. Open PowerShell.' + #13#10 +
      '2. Run: irm https://claude.ai/install.ps1 | iex' + #13#10 +
      '3. Run: claude' + #13#10 +
      '4. Follow the sign-in prompts (use /login if it doesn''t ask automatically).';
  end;

  ClaudeSetupMemo := TNewMemo.Create(ClaudeSetupPage);
  ClaudeSetupMemo.Parent := ClaudeSetupPage.Surface;
  ClaudeSetupMemo.Left := 0;
  ClaudeSetupMemo.Top := 0;
  ClaudeSetupMemo.Width := ClaudeSetupPage.SurfaceWidth;
  ClaudeSetupMemo.Height := ScaleY(160);
  ClaudeSetupMemo.ScrollBars := ssVertical;
  ClaudeSetupMemo.ReadOnly := True;
  ClaudeSetupMemo.Text := InfoText;

  ClaudeSetupCheckBox := TNewCheckBox.Create(ClaudeSetupPage);
  ClaudeSetupCheckBox.Parent := ClaudeSetupPage.Surface;
  ClaudeSetupCheckBox.Left := 0;
  ClaudeSetupCheckBox.Top := ClaudeSetupMemo.Top + ClaudeSetupMemo.Height + ScaleY(12);
  ClaudeSetupCheckBox.Width := ClaudeSetupPage.SurfaceWidth;
  ClaudeSetupCheckBox.Height := ScaleY(17);
  ClaudeSetupCheckBox.Checked := False;
  if ActiveLanguage = 'spanish' then
    ClaudeSetupCheckBox.Caption := 'Ya realice este paso (o ya tenia Claude Code configurado)'
  else
    ClaudeSetupCheckBox.Caption := 'I''ve completed this step (or already had Claude Code set up)';
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = ClaudeSetupPage.ID) and (not ClaudeSetupCheckBox.Checked) then
  begin
    if ActiveLanguage = 'spanish' then
      MsgBox('Debes confirmar que completaste este paso antes de continuar.', mbError, MB_OK)
    else
      MsgBox('You must confirm you''ve completed this step before continuing.', mbError, MB_OK);
    Result := False;
  end;
end;
