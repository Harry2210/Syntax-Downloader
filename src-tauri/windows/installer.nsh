!include "StrFunc.nsh"
${StrRep}

!macro NSIS_HOOK_POSTINSTALL
  ${StrRep} $0 "$INSTDIR" "\" "\\"
  FileOpen $1 "$INSTDIR\syntax-downloader-native-host.json" "w"
  FileWrite $1 "{$\"name$\":$\"com.syntax.downloader$\",$\"description$\":$\"Syntax Downloader browser launcher$\",$\"path$\":$\"$0\\syntax-downloader.exe$\",$\"type$\":$\"stdio$\",$\"allowed_origins$\":[$\"chrome-extension://bipnlkimdcbalpjhfdohfkglafnikjla/$\"]}"
  FileClose $1
  WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\com.syntax.downloader" "" "$INSTDIR\syntax-downloader-native-host.json"
  WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\com.syntax.downloader" "" "$INSTDIR\syntax-downloader-native-host.json"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\com.syntax.downloader"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\com.syntax.downloader"
  Delete "$INSTDIR\syntax-downloader-native-host.json"
!macroend
