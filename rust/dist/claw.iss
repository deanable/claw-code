#ifndef AppVersion
#error AppVersion must be defined on the command line
#endif

#ifndef SourceRoot
#error SourceRoot must be defined on the command line
#endif

#ifndef OutputDir
#error OutputDir must be defined on the command line
#endif

[Setup]
AppId={{8EDC86C9-7D4D-4DB2-8A08-0E8F1A6B4A2B}}
AppName=Claw Code
AppVersion={#AppVersion}
AppVerName=Claw Code {#AppVersion}
AppPublisher=Dean Kruger
AppPublisherURL=https://github.com/deanable/claw-code
AppSupportURL=https://github.com/deanable/claw-code
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Claw Code
DefaultGroupName=Claw Code
OutputDir={#OutputDir}
OutputBaseFilename=claw-{#AppVersion}-windows-x64-installer
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64
DisableDirPage=no
DisableProgramGroupPage=no
SetupLogging=yes
UninstallDisplayIcon={app}\claw-launcher.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#SourceRoot}\claw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceRoot}\claw-launcher.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceRoot}\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceRoot}\.claw\*"; DestDir: "{app}\.claw"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\Claw Launcher"; Filename: "{app}\claw-launcher.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\Claw Launcher"; Filename: "{app}\claw-launcher.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\claw-launcher.exe"; Description: "Launch Claw Launcher"; Flags: postinstall nowait skipifsilent
