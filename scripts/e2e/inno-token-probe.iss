{ Included by the inert Inno array probe. Compare only access rights on
  duplicates of our own primary token; never enable privileges or use a
  different user's token. The owned cmd.exe children have no application
  payload and are suspended only long enough to inspect their identity. }
type
  TProbeTokenBuffer = array[0..255] of Cardinal;
  TProbeTokenStartup = record
    Size, Reserved, Desktop, Title: Cardinal;
    X, Y, XSize, YSize, XCount, YCount, FillAttribute, Flags: Cardinal;
    ShowWindow, ReservedSize: Word;
    ReservedBytes, StandardInput, StandardOutput, StandardError: Cardinal;
  end;
  TProbeTokenProcess = record
    Process, Thread: THandle;
    ProcessId, ThreadId: Cardinal;
  end;

function ProbeOpenToken(Process: THandle; Access: Cardinal; var Token: THandle): Boolean;
  external 'OpenProcessToken@advapi32.dll stdcall';
function ProbeDuplicateToken(Token: THandle; Access, Attributes: Cardinal;
  Level, TokenType: Integer; var Duplicate: THandle): Boolean;
  external 'DuplicateTokenEx@advapi32.dll stdcall';
function ProbeTokenInformation(Token: THandle; Information: Integer;
  var Buffer: TProbeTokenBuffer; Size: Cardinal; var Needed: Cardinal): Boolean;
  external 'GetTokenInformation@advapi32.dll stdcall';
function ProbeEnvironment: Cardinal;
  external 'GetEnvironmentStringsW@kernel32.dll stdcall';
function ProbeFreeEnvironment(Environment: Cardinal): Boolean;
  external 'FreeEnvironmentStringsW@kernel32.dll stdcall';
function ProbeCreateWithToken(Token: THandle; LogonFlags: Cardinal;
  Application, Command: String; CreationFlags, Environment: Cardinal; Directory: String;
  var Startup: TProbeTokenStartup; var Process: TProbeTokenProcess): Boolean;
  external 'CreateProcessWithTokenW@advapi32.dll stdcall';
function ProbeResumeThread(Thread: THandle): Cardinal;
  external 'ResumeThread@kernel32.dll stdcall';
function ProbeExitCode(Process: THandle; var Code: Cardinal): Boolean;
  external 'GetExitCodeProcess@kernel32.dll stdcall';
function ProbeTerminateOwned(Process: THandle; Code: Cardinal): Boolean;
  external 'TerminateProcess@kernel32.dll stdcall';

function ProbeTokenError(const Operation: String): String;
var
  ErrorCode: Cardinal;
begin
  ErrorCode := DLLGetLastError;
  Result := Operation + ';win32=' + IntToStr(ErrorCode);
end;

procedure ProbeCloseTokenHandle(const Name: String; Handle: THandle);
var
  Closed: Boolean;
  Failure: String;
begin
  Closed := ProbeCloseHandle(Handle);
  if not Closed then begin
    Failure := ProbeTokenError('CloseHandle');
    Log('[token-probe] cleanup=' + Name + ';' + Failure);
  end;
  ProbeObserve(Name, Ord(Closed), 1, True);
end;

procedure ProbeReleaseTokenEnvironment(const Name: String; Environment: Cardinal);
var
  Released: Boolean;
  Failure: String;
begin
  Released := ProbeFreeEnvironment(Environment);
  if not Released then begin
    Failure := ProbeTokenError('FreeEnvironmentStringsW');
    Log('[token-probe] cleanup=' + Name + ';' + Failure);
  end;
  ProbeObserve(Name, Ord(Released), 1, True);
end;

procedure ProbeReadToken(Token: THandle; Information: Integer; var Buffer: TProbeTokenBuffer);
var
  Needed: Cardinal;
begin
  Needed := 0;
  if not ProbeTokenInformation(Token, Information, Buffer, 1024, Needed) then
    RaiseException(ProbeTokenError('GetTokenInformation/' + IntToStr(Information)));
  if (Needed = 0) or (Needed > 1024) then
    RaiseException('The token snapshot did not fit its fixed buffer.');
end;

function ProbeTokenSnapshot(Token: THandle): String;
var
  Buffer: TProbeTokenBuffer;
begin
  { Keep only pointer-free information in Pascal Script's fixed buffer. The
    application acceptance harness separately compares user and integrity SIDs. }
  ProbeReadToken(Token, 20, Buffer);
  Result := 'elevated=' + IntToStr(Buffer[0]);
  ProbeReadToken(Token, 12, Buffer);
  Result := Result + ';session=' + IntToStr(Buffer[0]);
  ProbeReadToken(Token, 10, Buffer);
  Result := Result + ';logon-low=' + IntToStr(Buffer[2]) +
    ';logon-high=' + IntToStr(Buffer[3]) + ';token-type=' + IntToStr(Buffer[6]);
end;

procedure ProbeTokenCase(OwnToken: THandle; Access: Cardinal; const OwnSnapshot: String);
var
  Duplicate, ChildToken: THandle;
  Environment, WaitResult, Code, ResumeResult: Cardinal;
  Startup: TProbeTokenStartup;
  Process: TProbeTokenProcess;
  CaseName, Target, BeforeSnapshot, ChildSnapshot, Failure: String;
  Required, Created, Exited: Boolean;
begin
  CaseName := 'token-mask-' + IntToStr(Access);
  Required := Access = $18B;
  Duplicate := 0;
  Environment := 0;
  Created := False;
  Exited := False;
  Log('[token-probe] mask=' + IntToStr(Access) + ';begin=1');
  try
    if not ProbeDuplicateToken(OwnToken, Access, 0, 2, 1, Duplicate) then begin
      Failure := ProbeTokenError('DuplicateTokenEx');
      Log('[token-probe] mask=' + IntToStr(Access) + ';' + Failure);
      ProbeObserve(CaseName + '-duplicate', 0, 1, Required);
      Exit;
    end;
    ProbeObserve(CaseName + '-duplicate', 1, 1, Required);
    BeforeSnapshot := ProbeTokenSnapshot(Duplicate);
    Log('[token-probe] mask=' + IntToStr(Access) + ';duplicate;' + BeforeSnapshot);
    ProbeObserve(CaseName + '-duplicate-identity', Ord(BeforeSnapshot = OwnSnapshot), 1, Required);
    Environment := ProbeEnvironment;
    if Environment = 0 then RaiseException(ProbeTokenError('GetEnvironmentStringsW'));
    Startup.Size := 68;
    Startup.Reserved := 0; Startup.Desktop := 0; Startup.Title := 0;
    Startup.X := 0; Startup.Y := 0; Startup.XSize := 0; Startup.YSize := 0;
    Startup.XCount := 0; Startup.YCount := 0; Startup.FillAttribute := 0; Startup.Flags := 0;
    Startup.ShowWindow := 0; Startup.ReservedSize := 0; Startup.ReservedBytes := 0;
    Startup.StandardInput := 0; Startup.StandardOutput := 0; Startup.StandardError := 0;
    Target := ExpandConstant('{sys}\cmd.exe');
    { Match production's Unicode environment and ABI. CREATE_SUSPENDED only
      keeps the short-lived child alive until its token has been inspected. }
    Created := ProbeCreateWithToken(Duplicate, 0, Target,
      '"' + Target + '" /d /c exit 0', $404, Environment,
      ExtractFileDir(Target), Startup, Process);
    if not Created then begin
      Failure := ProbeTokenError('CreateProcessWithTokenW');
      Log('[token-probe] mask=' + IntToStr(Access) + ';' + Failure);
      ProbeObserve(CaseName + '-created', 0, 1, Required);
      Exit;
    end;
    ProbeObserve(CaseName + '-created', 1, 1, Required);
    Log('[token-probe] mask=' + IntToStr(Access) + ';child-pid=' + IntToStr(Process.ProcessId));
    ChildToken := 0;
    if not ProbeOpenToken(Process.Process, 8, ChildToken) then
      RaiseException(ProbeTokenError('OpenProcessToken/child'));
    try
      ChildSnapshot := ProbeTokenSnapshot(ChildToken);
      Log('[token-probe] mask=' + IntToStr(Access) + ';child;' + ChildSnapshot);
      ProbeObserve(CaseName + '-child-identity', Ord(ChildSnapshot = OwnSnapshot), 1, Required);
    finally
      ProbeCloseTokenHandle(CaseName + '-child-token-closed', ChildToken);
    end;
    ResumeResult := ProbeResumeThread(Process.Thread);
    if ResumeResult = $FFFFFFFF then RaiseException(ProbeTokenError('ResumeThread'));
    ProbeObserve(CaseName + '-suspended-once', ResumeResult, 1, Required);
    WaitResult := ProbeWait(Process.Process, 2000);
    if WaitResult = $FFFFFFFF then RaiseException(ProbeTokenError('WaitForSingleObject/child'));
    Exited := WaitResult = 0;
    ProbeObserve(CaseName + '-exited', Ord(Exited), 1, Required);
    if Exited then begin
      if not ProbeExitCode(Process.Process, Code) then
        RaiseException(ProbeTokenError('GetExitCodeProcess'));
      ProbeObserve(CaseName + '-exit-code', Code, 0, Required);
    end;
  finally
    if Created then begin
      if not Exited then begin
        Log('[token-probe] mask=' + IntToStr(Access) + ';terminating-owned-child=1');
        if not ProbeTerminateOwned(Process.Process, 1) then begin
          Failure := ProbeTokenError('TerminateProcess/owned-child');
          Log('[token-probe] mask=' + IntToStr(Access) + ';' + Failure);
        end;
        WaitResult := ProbeWait(Process.Process, 2000);
        if WaitResult = $FFFFFFFF then begin
          Failure := ProbeTokenError('WaitForSingleObject/cleanup');
          Log('[token-probe] mask=' + IntToStr(Access) + ';' + Failure);
        end;
        ProbeObserve(CaseName + '-cleanup-exited', Ord(WaitResult = 0), 1, True);
      end;
      ProbeCloseTokenHandle(CaseName + '-child-thread-closed', Process.Thread);
      ProbeCloseTokenHandle(CaseName + '-child-process-closed', Process.Process);
    end;
    if Environment <> 0 then
      ProbeReleaseTokenEnvironment(CaseName + '-environment-released', Environment);
    if Duplicate <> 0 then ProbeCloseTokenHandle(CaseName + '-duplicate-closed', Duplicate);
  end;
end;

procedure ProbeTryTokenCase(OwnToken: THandle; Access: Cardinal; const OwnSnapshot: String);
begin
  try
    ProbeTokenCase(OwnToken, Access, OwnSnapshot);
  except
    Log('[token-probe] mask=' + IntToStr(Access) + ';exception=' + GetExceptionMessage);
    ProbeObserve('token-mask-' + IntToStr(Access) + '-exception-free', 0, 1, Access = $18B);
  end;
end;

procedure ProbeRunTokenCases;
var
  OwnToken: THandle;
  OwnSnapshot: String;
begin
  if not ProbeOpenToken(THandle(-1), $B, OwnToken) then
    RaiseException(ProbeTokenError('OpenProcessToken/self'));
  try
    OwnSnapshot := ProbeTokenSnapshot(OwnToken);
    Log('[token-probe] own;' + OwnSnapshot);
    ProbeTryTokenCase(OwnToken, $B, OwnSnapshot);
    ProbeTryTokenCase(OwnToken, $8B, OwnSnapshot);
    ProbeTryTokenCase(OwnToken, $10B, OwnSnapshot);
    ProbeTryTokenCase(OwnToken, $18B, OwnSnapshot);
  finally
    ProbeCloseTokenHandle('token-own-handle-closed', OwnToken);
  end;
end;
