; The compiler embeds its verified helper for maintenance before touching an
; existing installation. No user-supplied command, destination or driver runs.
; Native Windows NSIS splits File paths on backslashes; POSIX builds convert them.
!define EINK_BRIDGE_HELPER "${__FILEDIR__}\..\..\..\src-tauri\windows\bridge\eink-bridge-helper.exe"
Var EinkBridgeInstallerMutex

; Hold this handle until this installer/uninstaller process exits. The helper's
; own mutex serializes maintenance calls, while this one also covers replacing
; package files between those calls. An installer-triggered old uninstaller
; finishes before the new installation section acquires its lock.
!macro EINK_BRIDGE_PACKAGE_LOCK
  System::Call 'kernel32::CreateMutexW(p 0, i 1, w "Global\net.serialx.eink-qemu.bridge.installer") p.r0 ?e'
  Pop $1
  ${If} $0 = 0
  ${OrIf} $1 = 183
    SetErrorLevel 1
    MessageBox MB_ICONSTOP|MB_OK "Another eink-qemu installation or removal is running. Wait for it to finish, then retry." /SD IDOK
    Abort
  ${EndIf}
  StrCpy $EinkBridgeInstallerMutex $0
!macroend

!macro EINK_BRIDGE_MAINTAIN ACTION
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=eink-bridge-maintenance.exe "${EINK_BRIDGE_HELPER}"
  StrCpy $0 1
  ClearErrors
  ExecWait '"$PLUGINSDIR\eink-bridge-maintenance.exe" --maintain ${ACTION}' $0
  ${If} ${Errors}
  ${OrIf} $0 != 0
    SetErrorLevel 1
    MessageBox MB_ICONSTOP|MB_OK "Bridge support could not complete package maintenance. Close running emulators and review bridge setup, then retry. Its helper and recovery history must remain installed until cleanup finishes." /SD IDOK
    Abort
  ${EndIf}
  SetOutPath $INSTDIR
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro EINK_BRIDGE_PACKAGE_LOCK
  ${If} $INSTDIR != "$PROGRAMFILES64\eink-qemu"
    SetErrorLevel 1
    Abort "Bridge support requires the fixed Program Files installation directory."
  ${EndIf}
  !insertmacro EINK_BRIDGE_MAINTAIN prepare-update
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro EINK_BRIDGE_MAINTAIN install
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro EINK_BRIDGE_PACKAGE_LOCK
  ${If} $INSTDIR != "$PROGRAMFILES64\eink-qemu"
    SetErrorLevel 1
    Abort "The installation location does not match this bridge support package."
  ${EndIf}
  ${If} $UpdateMode = 1
    !insertmacro EINK_BRIDGE_MAINTAIN prepare-update
  ${Else}
    !insertmacro EINK_BRIDGE_MAINTAIN remove
  ${EndIf}
!macroend
