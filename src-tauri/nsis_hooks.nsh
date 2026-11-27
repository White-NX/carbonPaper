; beforeBundleCommand creates the manifest files before Tauri discovers resources.
; Tauri then patches carbonpaper.exe with the NSIS bundle type. Re-sign at
; compile time, before any File instruction captures the final runtime bytes.
!system 'node "${__FILEDIR__}\..\scripts\sign-protected-runtime.mjs"' = 0

!macro NSIS_HOOK_POSTINSTALL
  ; Files that releases with the Python monitor installed. An upgrade only
  ; overwrites what the new bundle carries, so these would otherwise stay.
  ; The Python environment itself is removed by the application at runtime
  ; (legacy_python_cleanup.rs), off the installer's critical path.
  RmDir /r "$INSTDIR\monitor"
  Delete "$INSTDIR\monitor.pyz"
  Delete "$INSTDIR\carbonpaper-python.exe"
  Delete "$INSTDIR\python-3.12.10-amd64.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; An updater's temporary uninstall keeps the protected component. A full
  ; uninstall retires this user's grants before deleting its signed entry point.
  ${If} $UpdateMode <> 1
    IfFileExists "$INSTDIR\carbonpaper.exe" 0 carbonpaper_protected_uninstall_done
    ClearErrors
    ExecWait '"$INSTDIR\carbonpaper.exe" --uninstall-protected-component' $0
    ${If} ${Errors}
      Abort
    ${EndIf}
    ${If} $0 <> 0
      Abort
    ${EndIf}
  ${EndIf}
  carbonpaper_protected_uninstall_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RmDir /r "$LOCALAPPDATA\CarbonPaper\.venv"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\.venv.removing"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\models"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\model"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\data"
    RmDir "$LOCALAPPDATA\CarbonPaper"
  ${EndIf}
!macroend
