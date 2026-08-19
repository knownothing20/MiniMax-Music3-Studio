; Installer hooks for the NSIS package.
;
; The executable used to be named after the Rust crate -
; minimax-music3-studio-desktop.exe - while the portable build named the same
; binary MiniMax-Music3-Studio.exe. One studio, two names, depending on how it
; was installed. The name is now the same in both, and this removes the old one
; so an updated installation does not keep a dead 52 MB copy and a shortcut
; pointing at whichever the user happened to click first.

!macro NSIS_HOOK_PREINSTALL
  Delete "$INSTDIR\minimax-music3-studio-desktop.exe"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$INSTDIR\minimax-music3-studio-desktop.exe"
!macroend
