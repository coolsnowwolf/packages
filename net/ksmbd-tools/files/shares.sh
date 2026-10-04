#!/bin/sh
# Home shares remain private runtime entries; mount shares are stored in UCI.
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

# Use a private delta directory so commits do not include pending LuCI edits.
ksmbd_mount_uci()
{
 uci ${UCI_CONFIG_DIR:+-c "$UCI_CONFIG_DIR"} -t "$ksmbd_delta" -q "$@"
}

ksmbd_prune_mount()
{
 local section="$1" path auto_path fingerprint
 config_get path "$section" path
 config_get auto_path "$section" auto_path
 [ -n "$auto_path" ] || return 0
 if [ "$path" != "$auto_path" ]; then
  # Editing the path converts an automatic share into a manual share.
  ksmbd_mount_uci delete "ksmbd.$section.auto_path"
 elif [ "$KSMBD_AUTOSHARE" -ne 1 ] || ! grep -Fxq "$path" "$ksmbd_mounts"; then
  ksmbd_mount_uci delete "ksmbd.$section"
 else
  # Only remove identical generated entries; differing permissions are
  # intentional configuration and must not be silently discarded.
  fingerprint="$(ksmbd_mount_uci show "ksmbd.$section" |
   sed '1d; s/^ksmbd\.[^.]*\.//' | LC_ALL=C sort | md5sum | cut -d ' ' -f 1)"
  if grep -Fxq "$fingerprint" "$ksmbd_delta/seen"; then
   ksmbd_mount_uci delete "ksmbd.$section"
  else
   printf '%s\n' "$fingerprint" >> "$ksmbd_delta/seen"
  fi
 fi
 return 0
}

ksmbd_find_mount()
{
 local section="$1" share_name share_path
 config_get share_name "$section" name
 config_get share_path "$section" path
 if [ "$share_path" = "$target" ] ||
    [ "$(printf '%s' "$share_name" | tr 'A-Z' 'a-z')" = "$(printf '%s' "$name" | tr 'A-Z' 'a-z')" ]; then
  ksmbd_found=1
 fi
}

ksmbd_scan_mounts()
{
 local device target fstype options rest name
 while read -r device target fstype options rest; do
  case "$device" in /dev/sd*|/dev/hd*|/dev/mmcblk*|/dev/nvme*|/dev/md*) ;; *) continue;; esac
  # Firmware images and other read-only mounts are not automatic shares.
  case "$fstype" in squashfs|erofs|cramfs|romfs|iso9660) continue;; esac
  case ",$options," in *,ro,*) continue;; esac
  target="$(printf '%b' "$target")"
  case "$target" in /|/rom|/overlay|/boot|/boot/*|/tmp|/tmp/*|/dev|/dev/*|/proc|/proc/*|/sys|/sys/*) continue;; esac
  case "$target" in *'
'*|*'	'*) continue;; esac
  [ -d "$target" ] || continue
  name="${target##*/}"
  case "$name" in ''|*'['*|*']'*|*';'*|*'%'*) name="${device##*/}";; esac
  printf '%s\t%s\tno\n' "$target" "$name"
 done < /proc/mounts
}

ksmbd_sync_mounts()
(
 local ksmbd_delta ksmbd_mounts target name readonly section ksmbd_found
 flock -x 8 || return 1
 # The caller's snapshot can predate another hotplug worker's commit.
 config_load ksmbd
 ksmbd_delta="$(mktemp -d /tmp/ksmbd-uci.XXXXXX)" || return 1
 trap 'rm -rf "$ksmbd_delta"' EXIT
 : > "$ksmbd_delta/seen"
 ksmbd_mounts="$ksmbd_delta/paths"
 ksmbd_scan_mounts > "$ksmbd_delta/mounts"
 cut -f1 "$ksmbd_delta/mounts" > "$ksmbd_mounts"
 config_foreach ksmbd_prune_mount share
 if [ -n "$(ksmbd_mount_uci changes ksmbd)" ]; then
  ksmbd_mount_uci commit ksmbd || return 1
 fi
 [ "$KSMBD_AUTOSHARE" -eq 1 ] || return 0
 config_load ksmbd
 while IFS="$(printf '\t')" read -r target name readonly; do
  ksmbd_found=0
  config_foreach ksmbd_find_mount share
  [ "$ksmbd_found" -eq 0 ] || continue
  section="$(ksmbd_mount_uci add ksmbd share)" || return 1
  ksmbd_mount_uci set "ksmbd.$section.name=$name"
  ksmbd_mount_uci set "ksmbd.$section.path=$target"
  ksmbd_mount_uci set "ksmbd.$section.auto_path=$target"
  ksmbd_mount_uci set "ksmbd.$section.browseable=yes"
  ksmbd_mount_uci set "ksmbd.$section.guest_ok=yes"
  ksmbd_mount_uci set "ksmbd.$section.read_only=$readonly"
  ksmbd_mount_uci set "ksmbd.$section.force_root=1"
  ksmbd_mount_uci set "ksmbd.$section.create_mask=0666"
  ksmbd_mount_uci set "ksmbd.$section.dir_mask=0777"
  ksmbd_mount_uci commit ksmbd || return 1
  config_load ksmbd
 done < "$ksmbd_delta/mounts"
) 8>/var/lock/ksmbd-autoshare.lock
