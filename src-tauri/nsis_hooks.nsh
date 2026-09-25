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
