; Inno Setup 6 script for snipdrop.
;
; Build:   cargo build --release
;          iscc installer\snipdrop.iss
; Output:  target\installer\snipdrop-<version>-setup.exe
;
; Installs for the current user only, so it never asks for administrator rights.

#define AppName "snipdrop"
#define AppExe "..\target\release\snipdrop.exe"
#define AppVersion GetStringFileInfo(AppExe, "ProductVersion")
#define AppUrl "https://github.com/jvadura/snipdrop"
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"

#if AppVersion == ""
  #error The release executable is missing or has no version info. Run "cargo build --release" first.
#endif

[Setup]
; Never change AppId: it is how upgrades find the previous installation.
AppId={{6B0C7E0B-6C2B-4A55-9B7F-2B3B1D0E5A41}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=jvadura
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\target\installer
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=..\assets\snipdrop.ico
UninstallDisplayIcon={app}\snipdrop.exe
UninstallDisplayName={#AppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; The running copy is closed by the [Code] section below, which knows how to
; ask a tray-only program to exit; Restart Manager is not needed.
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start {#AppName} when I sign in to Windows"
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; DestName: "README.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\snipdrop.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\snipdrop.exe"; Tasks: desktopicon

[Registry]
; The same value the tray menu's "Start with Windows" switch reads and writes,
; so the installer option and the in-app switch always agree.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\snipdrop.exe"""; Tasks: autostart
Root: HKCU; Subkey: "{#RunKey}"; ValueType: none; ValueName: "{#AppName}"; Flags: deletevalue; Tasks: not autostart

[Run]
Filename: "{app}\snipdrop.exe"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; The error log. The config in %APPDATA%\snipdrop and the saved snips are the
; user's and are left alone.
Type: filesandordirs; Name: "{localappdata}\{#AppName}"

[Code]
const
  CloseMessage = $0010; { WM_CLOSE }
  RunningMutex = 'Local\snipdrop.single-instance';

{ Ask a running snipdrop to exit and wait until it has. It has no visible
  window, so it is found through its hidden main window's class. }
procedure CloseRunningApp;
var
  Window: HWND;
  Attempt: Integer;
begin
  Window := FindWindowByClassName('snipdrop.main');
  if Window <> 0 then
    PostMessage(Window, CloseMessage, 0, 0);
  for Attempt := 1 to 50 do
  begin
    if not CheckForMutexes(RunningMutex) then
      Exit;
    Sleep(100);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseRunningApp;
  Result := '';
end;

function InitializeUninstall: Boolean;
begin
  CloseRunningApp;
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { Autostart may have been switched on from the tray menu after installing,
    so it is removed here rather than relying on what Setup itself wrote. }
  if CurUninstallStep = usUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#AppName}');
end;
