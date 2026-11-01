@echo off
rem Weekly cleanup on the infra host (Windows Task Scheduler "MoriaPrune", Sunday 04:00).
rem Removes Moria images that no container uses and that are older than 7 days (CI pins
rem a new tag on every deploy), plus build cache older than 7 days.
rem Install: scp docker\prune.cmd this@192.168.2.10:moria-prune.cmd
rem   then ssh this@192.168.2.10 schtasks /create /f /tn MoriaPrune /sc weekly /d SUN /st 04:00 /tr "%USERPROFILE%\moria-prune.cmd"
docker image prune -af --filter "label=org.opencontainers.image.source=https://github.com/nicolaslallier/Moria" --filter "until=168h"
docker builder prune -f --filter "until=168h"
