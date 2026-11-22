#ifndef AppVersion
  #error AppVersion must be supplied by scripts/build-installer.ps1
#endif
#ifndef ApplicationBinary
  #error ApplicationBinary must be supplied by scripts/build-installer.ps1
#endif
#ifndef InstallerOutputDirectory
  #error InstallerOutputDirectory must be supplied by scripts/build-installer.ps1
#endif
#define AppName "Rust Switcher"
#define AppExeName "rust-switcher.exe"
#define PackageIdentifier "QQRM.RustSwitcher"
#define RunValueName "RustSwitcher"
#define AppId "QQRM.RustSwitcher"
#define RestartArguments "--autostart --skip-startup-update --ensure-running"
#define QuitPidArgument "--parent-pid "
#define AutostartArguments "--autostart"
#define DefaultInstallDirectory "{localappdata}\Programs\RustSwitcher"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
SetupMutex=Local\QQRM.RustSwitcher.Setup
AppPublisher=QQRM
AppPublisherURL=https://github.com/qqrm
AppSupportURL=https://github.com/qqrm/rust-switcher-rs/issues
AppUpdatesURL=https://github.com/qqrm/rust-switcher-rs/releases/latest
DefaultDirName={code:GetDefaultInstallDirectory}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
OutputDir={#InstallerOutputDirectory}
OutputBaseFilename=rust-switcher-setup
SetupIconFile=..\..\res\windows\icon_transparent.ico
UninstallDisplayIcon={app}\{#AppExeName}
Compression=lzma2
SolidCompression=yes
CloseApplications=no
RestartApplications=no
RestartIfNeededByRun=no
ChangesEnvironment=yes
WizardStyle=modern
LicenseFile=..\..\LICENSE

[Files]
Source: "{#ApplicationBinary}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"

[Registry]
Root: HKCU64; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1"; ValueType: string; ValueName: "WinGetInstallerType"; ValueData: "inno"; Flags: uninsdeletevalue

[Code]
#include "app-lifecycle.iss"
