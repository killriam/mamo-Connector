; Mamo Connector Installer Script for Inno Setup
; Download Inno Setup from: https://jrsoftware.org/isinfo.php

#define MyAppName "Mamo Connector"
#define MyAppVersion "0.3.17"
#define MyAppPublisher "Mamo Connector Team"
#define MyAppURL "https://github.com/killriam/mamo-Connector"
#define MyAppExeName "mamo-connector.exe"

[Setup]
; Unique Application ID - DO NOT change this between versions
AppId={{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
; Output settings
OutputDir=..\target\installer
OutputBaseFilename=MamoConnector-{#MyAppVersion}-Setup
; Compression
Compression=lzma2
SolidCompression=yes
; Require admin for protocol registration (optional, can be user-level)
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Modern look
WizardStyle=modern
; License file (optional)
; LicenseFile=..\LICENSE
; Icon (optional - create an icon file)
; SetupIconFile=..\assets\icon.ico

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "registerprotocol"; Description: "Register mamoConnector:// protocol handler"; GroupDescription: "Protocol Handler:"; Flags: checkedonce

[Files]
; Main executable
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
; Include any additional files here (DLLs, config files, etc.)
; Source: "..\config\*"; DestDir: "{app}\config"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
; Register the custom URL protocol (mamoConnector://)
Root: HKCU; Subkey: "Software\Classes\mamoConnector"; ValueType: string; ValueName: ""; ValueData: "URL:Mamo Connector Protocol"; Flags: uninsdeletekey; Tasks: registerprotocol
Root: HKCU; Subkey: "Software\Classes\mamoConnector"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Tasks: registerprotocol
Root: HKCU; Subkey: "Software\Classes\mamoConnector\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: registerprotocol

[Run]
; Option to run after installation
Filename: "{app}\{#MyAppExeName}"; Description: "Launch MaMo Connector and set up Forge (recommended)"; Flags: nowait postinstall skipifsilent

[Code]
// Custom code to show installation success message
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Installation complete
  end;
end;
