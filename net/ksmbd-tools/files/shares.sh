#!/bin/sh
# Runtime-only shares; explicit names and paths take precedence.
ksmbd_share_exists()
{
 KSMBD_SHARE_NAME="$1" KSMBD_SHARE_PATH="$2" awk '
 /^\[/ { name = substr($0, 2, length($0)-2); if (tolower(name) == tolower(ENVIRON["KSMBD_SHARE_NAME"])) found=1 }
 /^[ \t]*path[ \t]*=/ { path=$0; sub(/^[ \t]*path[ \t]*=[ \t]*/, "", path); if (path == ENVIRON["KSMBD_SHARE_PATH"]) found=1 }
 END { exit !found }' /var/etc/ksmbd/ksmbd.conf
}

ksmbd_add_homes()
{
 local user password uid gid gecos home shell
 [ -r /etc/ksmbd/ksmbdpwd.db ] || return 0
 while IFS=: read -r user password uid gid gecos home shell; do
  case "$user" in ''|*[!a-zA-Z0-9_.-]*) continue;; esac
  case "$home" in /|/var|/tmp|/dev|/proc|/sys|'') continue;; /*) ;; *) continue;; esac
  [ -d "$home" ] || continue
  # Private homes are only generated for registered SMB users.
  awk -F: -v user="$user" '$1 == user { found=1 } END { exit !found }' /etc/ksmbd/ksmbdpwd.db || continue
  ksmbd_share_exists "$user" "$home" && continue
  {
   printf '\n[%s]\n\tpath = %s\n' "$user" "$home"
   printf '\tcomment = Home Directories\n\tvalid users = %s\n' "$user"
   printf '\tbrowseable = no\n\tguest ok = no\n\tread only = no\n'
   printf '\tcreate mask = 0750\n\tdirectory mask = 0750\n'
  } >> /var/etc/ksmbd/ksmbd.conf
 done < /etc/passwd
}

ksmbd_add_mounts()
{
 local device target fstype options rest name readonly
 while read -r device target fstype options rest; do
  case "$device" in /dev/sd*|/dev/hd*|/dev/mmcblk*|/dev/nvme*|/dev/md*) ;; *) continue;; esac
  target="$(printf '%b' "$target")"
  case "$target" in /|/rom|/overlay|/boot|/boot/*|/tmp|/tmp/*|/dev|/dev/*|/proc|/proc/*|/sys|/sys/*) continue;; esac
  case "$target" in *'
'*) continue;; esac
  [ -d "$target" ] || continue
  name="${target##*/}"
  case "$name" in ''|*'['*|*']'*|*';'*|*'%'*) name="${device##*/}";; esac
  ksmbd_share_exists "$name" "$target" && continue
  readonly=no
  case ",$options," in *,ro,*) readonly=yes;; esac
  {
   printf '\n[%s]\n\tpath = %s\n' "$name" "$target"
   printf '\tbrowseable = yes\n\tguest ok = yes\n\tread only = %s\n' "$readonly"
   printf '\tforce user = root\n\tforce group = root\n'
   printf '\tcreate mask = 0666\n\tdirectory mask = 0777\n'
  } >> /var/etc/ksmbd/ksmbd.conf
 done < /proc/mounts
}
