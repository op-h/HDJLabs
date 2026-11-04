; HDJLabs Studio installer (Inno Setup 6).
; Build:  ISCC.exe /DAppVersion=1.0.0-beta /DSourceExe=path\to\HDJLabs.exe installer\HDJLabs.iss
; Exported exams never need this — they are single portable .exe files.

#ifndef AppVersion
  #define AppVersion "1.0.1-beta"
#endif
#ifndef SourceExe
  #define SourceExe "..\target-win7\x86_64-win7-windows-msvc\release\HDJLabs.exe"
#endif

[Setup]
AppId={{6E1F4C2A-8B7D-4E35-9A61-HDJLABS00001}
AppName=HDJLabs Studio
AppVersion={#AppVersion}
AppVerName=HDJLabs Studio {#AppVersion}
AppPublisher=OPH
AppPublisherURL=https://github.com/op-h/HDJLabs
AppSupportURL=https://github.com/op-h/HDJLabs/issues
AppUpdatesURL=https://github.com/op-h/HDJLabs/releases
DefaultDirName={autopf}\HDJLabs
DefaultGroupName=HDJLabs
DisableProgramGroupPage=yes
; Per-user by default (no admin needed on school PCs); admins may install for everyone.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=6.1sp1
OutputDir=..\dist
OutputBaseFilename=HDJLabs-Setup
SetupIconFile=..\assets\hdjlabs.ico
UninstallDisplayIcon={app}\HDJLabs.exe
UninstallDisplayName=HDJLabs Studio
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "firewall"; Description: "Allow HDJLabs through Windows Firewall (needed for the Live classroom)"; GroupDescription: "Network:"; Check: IsAdminInstallMode

[InstallDelete]
; The animated guides were replaced by the in-app step-by-step guide (1.4).
Type: filesandordirs; Name: "{app}\guides"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "HDJLabs.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\assets\Phosphor-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\tools\lab-trust-teacher.ps1"; DestDir: "{app}\tools"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\HDJLabs Studio"; Filename: "{app}\HDJLabs.exe"; Comment: "Build and run secure exams"
Name: "{autodesktop}\HDJLabs Studio"; Filename: "{app}\HDJLabs.exe"; Tasks: desktopicon

[Registry]
; Double-clicking a .hdjx exam project opens it in the Studio.
Root: HKA; Subkey: "Software\Classes\.hdjx"; ValueType: string; ValueName: ""; ValueData: "HDJLabs.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\HDJLabs.Project"; ValueType: string; ValueName: ""; ValueData: "HDJLabs exam project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\HDJLabs.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\HDJLabs.exe,0"
Root: HKA; Subkey: "Software\Classes\HDJLabs.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\HDJLabs.exe"" ""%1"""

[Run]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""HDJLabs Studio"" dir=in action=allow program=""{app}\HDJLabs.exe"" enable=yes profile=any"; Flags: runhidden; Tasks: firewall
Filename: "{app}\HDJLabs.exe"; Description: "Open HDJLabs Studio"; Flags: nowait postinstall skipifsilent
; /RELAUNCH=1: open Studio again after a silent install.
Filename: "{app}\HDJLabs.exe"; Flags: nowait; Check: Relaunch

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""HDJLabs Studio"" program=""{app}\HDJLabs.exe"""; Flags: runhidden; RunOnceId: "HDJLabsFirewall"

[Code]
function Relaunch: Boolean;
begin
  Result := ExpandConstant('{param:relaunch|0}') = '1';
end;
