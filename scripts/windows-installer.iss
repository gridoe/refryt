; refryt's Windows installer. Compiled by scripts/build-windows.sh via ISCC under Wine — see
; docs/BUILDING.md. Version and the staged input folder are passed in with /D, not hardcoded,
; so this stays in sync with whatever the Rust build actually produced.
;
; Stable across releases so Windows treats a new installer as an *upgrade* of the same app
; rather than a second, separate install:
; A doubled leading brace is Inno Setup's own escape for a literal "{" — without it, the GUID
; below parses as a (nonexistent) named constant rather than a literal string.
#define AppId "{{534D3A53-19D8-4423-94FD-1AEE64DBD7C3}"

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
; The file name to write; build-windows.sh passes it.
#ifndef OutputName
  #define OutputName "refryt-" + AppVersion + "-windows-x86_64-setup"
#endif
#ifndef StageDir
  #define StageDir "stage"
#endif

[Setup]
AppId={#AppId}
AppName=refryt
AppVersion={#AppVersion}
AppPublisher=refryt
AppPublisherURL=https://github.com/gridoe/refryt
DefaultDirName={autopf}\refryt
DefaultGroupName=refryt
UninstallDisplayIcon={app}\refryt.exe
OutputDir=..\dist
OutputBaseFilename={#OutputName}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\LICENSE
; Shown before installing: what refryt connects to and how to turn each one off.
InfoBeforeFile=..\PRIVACY.md
DisableProgramGroupPage=yes
; No signing key exists for this project, so an unsigned installer will still trip Windows'
; SmartScreen the first few times anyone runs it — a known, disclosed limitation, not a bug.
SetupIconFile=..\assets\icons\app\refryt.ico

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "{#StageDir}\refryt.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
; The colour emoji font: Windows' own is COLR, which the emoji painter cannot read.
Source: "{#StageDir}\NotoColorEmoji.ttf"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\yt-dlp.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\refryt"; Filename: "{app}\refryt.exe"
Name: "{autodesktop}\refryt"; Filename: "{app}\refryt.exe"; Tasks: desktopicon
Name: "{group}\Uninstall refryt"; Filename: "{uninstallexe}"

[Run]
; `runasoriginaluser`: the setup is elevated (Program Files), and without this the post-install
; launch inherits that elevation. An elevated refryt is not just unnecessary — it breaks yt-dlp:
; its startup PATH scan for ffmpeg dies with `[WinError 448] ... untrusted mount point` the
; moment it meets a scoop-style junction, because Windows marks junctions made by a
; non-elevated process as untrusted for elevated ones. That read as "all resolvers failed" on
; the first launch after installing, and only that one.
Filename: "{app}\refryt.exe"; Description: "Launch refryt"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallDelete]
; Nothing under {app} to clean up beyond the files listed above — settings and the signed-in
; session live under the user's own AppData, same as on Linux they live under ~/.config, and an
; uninstall leaving those behind (rather than silently deleting a session) is the same choice
; every other app on the platform makes.
