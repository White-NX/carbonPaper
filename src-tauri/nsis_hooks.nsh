!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RmDir /r "$LOCALAPPDATA\CarbonPaper\.venv"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\models"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\model"
    RmDir /r "$LOCALAPPDATA\CarbonPaper\data"
    RmDir "$LOCALAPPDATA\CarbonPaper"
  ${EndIf}
!macroend
