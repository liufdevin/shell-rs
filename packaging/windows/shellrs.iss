; The Windows setup program: installs ShellRS for the current user, with no
; administrator rights, under %LOCALAPPDATA%\Programs\ShellRS.
;
;   iscc /DAppVersion=0.2.0 /DBuildDir=..\..\target\release /O..\..\dist shellrs.iss
;
; The same program updates an installed copy: ShellRS downloads the next
; version's setup and runs it with /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
; /SP- once it has exited, then starts itself again. AppId must never
; change, or the update installs a second copy beside the first.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef BuildDir
  #define BuildDir "..\..\target\release"
#endif
#define IconFile AddBackslash(SourcePath) + "..\..\assets\logo\shelllf.ico"
; Languages\ChineseSimplified.isl is UTF-8 without a BOM, and has the
; messages of 6.5 onwards.
#if Ver < EncodeVer(6, 5, 0)
  #error Inno Setup 6.5 or later is needed
#endif

[Setup]
AppId={{45D9E9F5-84BB-4B09-ABE8-1FFE107ACF96}
AppName=ShellLF
AppVersion={#AppVersion}
AppVerName=ShellLF {#AppVersion}
AppPublisher=ShellLF
AppPublisherURL=https://shellrs.com
AppSupportURL=https://shellrs.com
AppUpdatesURL=https://shellrs.com/download
DefaultDirName={autopf}\ShellLF
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputBaseFilename=ShellRS-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\shellrs.exe
UninstallDisplayName=ShellLF
; A running ShellRS is closed through the Restart Manager rather than
; making the setup fail on files in use.
CloseApplications=yes
; The setup speaks the language Windows shows, without asking: Simplified
; Chinese on a Chinese Windows, English on any other, as ShellRS itself.
ShowLanguageDialog=no
; Not the language of the copy it replaces: up to 0.1.9-beta.1 every setup
; was English, which would otherwise stay with each installed copy.
UsePreviousLanguage=no
#if FileExists(IconFile)
SetupIconFile={#IconFile}
#endif

[Languages]
; The first is the one for a language not listed.
Name: "english"; MessagesFile: "compiler:Default.isl"
; Not among the translations Inno Setup installs, so kept here: issrc's
; Files/Languages/Unofficial/ChineseSimplified.isl at is-6_7_1, the version
; on the windows-2025 runner. Update it with Inno Setup.
Name: "chinesesimplified"; MessagesFile: "Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BuildDir}\shellrs.exe"; DestDir: "{app}"; Flags: ignoreversion
; The `shellrs` command: 设置 › 外部 CLI copies it onto the PATH.
Source: "{#BuildDir}\shelllf-cli.exe"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
Type: files; Name: "{app}\shellrs-cli.exe"
Type: files; Name: "{autoprograms}\ShellRS.lnk"
Type: files; Name: "{autodesktop}\ShellRS.lnk"

[Icons]
; The AppUserModelID ShellRS gives itself (`set_app_identity`, for its
; notifications): the same here keeps a pinned shortcut and the running
; window one taskbar button.
Name: "{autoprograms}\ShellLF"; Filename: "{app}\shellrs.exe"; AppUserModelID: "com.shellrs.ShellRS"
Name: "{autodesktop}\ShellLF"; Filename: "{app}\shellrs.exe"; AppUserModelID: "com.shellrs.ShellRS"; Tasks: desktopicon

[Run]
; Interactive installs only: an update is restarted by ShellRS's own helper,
; which waits for this setup to finish.
Filename: "{app}\shellrs.exe"; Description: "{cm:LaunchProgram,ShellLF}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Files a later version added, which this uninstaller does not list.
Type: filesandordirs; Name: "{app}"
