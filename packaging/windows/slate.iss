#define AppVersion GetEnv("SLATE_VERSION")
[Setup]
AppId={{6E1B6A0C-6F52-4B0B-9B0E-5C1A57A7E001}
AppName=Slate
AppVersion={#AppVersion}
AppPublisher=Slate contributors
DefaultDirName={autopf}\Slate
DefaultGroupName=Slate
OutputDir=..\..\dist
OutputBaseFilename=slate-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\slate.exe
LicenseFile=..\..\LICENSE

[Files]
Source: "..\..\dist\slate-win\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\Slate"; Filename: "{app}\slate.exe"
Name: "{group}\Uninstall Slate"; Filename: "{uninstallexe}"

[Registry]
Root: HKA; Subkey: "Software\Classes\tidal"; ValueType: string; ValueData: "URL:TIDAL"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\tidal"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\tidal\shell\open\command"; ValueType: string; ValueData: """{app}\slate.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\slate"; ValueType: string; ValueData: "URL:Slate"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\slate"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\slate\shell\open\command"; ValueType: string; ValueData: """{app}\slate.exe"" ""%1"""
