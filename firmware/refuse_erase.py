# PlatformIO extra script for env heltec_v4 (the multimode build): `-t erase`
# would run esptool erase_flash and wipe every mode on the board (the
# bootloader, the picker, all four slots and their data). Refuse it, the same
# way upload_command refuses `-t upload`. The one time full wipe for the
# multimode install is done by hand (multimode/INSTALL.md).
Import("env")  # noqa: F821 (provided by PlatformIO)

env.Replace(  # noqa: F821
    ERASECMD='"$PYTHONEXE" -c "import sys; sys.exit(\'heltec_v4 is the multimode build. -t erase would wipe every mode on the board; it is refused. See multimode/INSTALL.md.\')"'
)
