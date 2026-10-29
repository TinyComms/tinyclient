; The Windows installer, built with Inno Setup 6 by packaging/windows.ps1. It installs the four
; programs side by side (each finds the others beside itself), for the current user by default so
; no administrator is needed, with a Start menu entry, an optional desktop shortcut, and an
; uninstaller listed in Settings > Apps.
;
;   iscc /DVersion=0.2.1 /DSource=..\..\target\release /DOutput=tinyclient-v0.2.1-windows-x86_64-setup packaging\windows\tinyclient.iss
;
; Silent: tinyclient-...-setup.exe /VERYSILENT /CURRENTUSER (or /ALLUSERS, which needs an administrator).

#ifndef Version
  #error Pass /DVersion=<x.y.z>
#endif
#ifndef Source
  #define Source "..\..\target\release"
#endif
#ifndef Output
  #define Output "tinyclient-setup"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
; Never change the AppId: Windows knows an installed tinyclient by it, for upgrades and uninstall.
AppId={{8C1B5E0A-4F3D-4C1E-9B7A-2D6E5F1A7C3B}
AppName=tinyclient
AppVersion={#Version}
AppVerName=tinyclient {#Version}
AppPublisher=tinyclient
AppPublisherURL=https://tinyclient.io
AppSupportURL=https://github.com/itz4blitz/tinyclient/issues
AppUpdatesURL=https://tinyclient.io/docs/#windows
VersionInfoVersion={#Version}
DefaultDirName={autopf}\tinyclient
DefaultGroupName=tinyclient
DisableProgramGroupPage=yes
DisableDirPage=auto
; For the current user (%LOCALAPPDATA%\Programs) unless an administrator chooses everyone.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=tinyclient.ico
UninstallDisplayIcon={app}\tinyclient.ico
UninstallDisplayName=tinyclient
OutputDir={#OutputDir}
OutputBaseFilename={#Output}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\tinyclient.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\chatd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\mediad.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\tinyclient-login.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "tinyclient.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\AGPL-3.0.txt"; DestDir: "{app}"; Flags: ignoreversion

; The Start menu shortcut carries the AppUserModelID chatd posts notifications under
; (chatd/src/notify.rs `app`): an unpackaged program's toasts are shown by the shortcut that
; names its ID. Never change it, or notifications already allowed in Windows Settings are lost.
[Icons]
Name: "{autoprograms}\tinyclient"; Filename: "{app}\tinyclient.exe"; IconFilename: "{app}\tinyclient.ico"; AppUserModelID: "dev.tinyclient.app"
Name: "{autodesktop}\tinyclient"; Filename: "{app}\tinyclient.exe"; IconFilename: "{app}\tinyclient.ico"; AppUserModelID: "dev.tinyclient.app"; Tasks: desktopicon

; The same ID's name and icon for the notification centre, so toasts show "tinyclient" and its
; icon whichever way it was started. Removed with the program.
[Registry]
Root: HKA; Subkey: "Software\Classes\AppUserModelId\dev.tinyclient.app"; ValueType: string; ValueName: "DisplayName"; ValueData: "tinyclient"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\AppUserModelId\dev.tinyclient.app"; ValueType: string; ValueName: "IconUri"; ValueData: "{app}\tinyclient.ico"

[Run]
Filename: "{app}\tinyclient.exe"; Description: "{cm:LaunchProgram,tinyclient}"; Flags: nowait postinstall skipifsilent

[Code]
// chatd outlives the window by design, and mediad may be in a call: stop whichever of them runs
// from this folder before its files are replaced or removed. Processes from anywhere else (a
// build, the portable zip) are left alone.
procedure StopRunning();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -NonInteractive -Command "Get-Process tinyclient,chatd,mediad,tinyclient-login -ErrorAction SilentlyContinue' +
    ' | Where-Object { $_.Path -like ''' + ExpandConstant('{app}') + '\*'' } | Stop-Process -Force"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopRunning();
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopRunning();
end;
