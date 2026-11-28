use super::request::Action;
use crate::mqtt_identity::Selection;
use fttr_api::{HlAccount as Account, HlAccountReply, HlWriteStatus};
use std::{io, time::Duration};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "account metadata response is incomplete or unsupported",
    )
}

fn parse_metadata(output: &str) -> io::Result<Vec<Account>> {
    let mut rows = Vec::new();
    for (index, line) in output.lines().enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        let entry = u8::try_from(index).map_err(|_| invalid())?;
        if fields.len() != 3
            || entry > 2
            || fields[0] != entry.to_string()
            || fields[1].is_empty()
            || fields[1].len() > 128
        {
            return Err(invalid());
        }
        let active = match fields[2] {
            "Yes" => true,
            "No" => false,
            _ => return Err(invalid()),
        };
        rows.push(Account {
            entry,
            username: fields[1].into(),
            active,
        });
    }
    if rows.len() != 3 {
        return Err(invalid());
    }
    Ok(rows)
}

fn mutation(action: &Action) -> io::Result<String> {
    let (entry, field, value) = match action {
        Action::AccountSetPassword { entry, password } => (*entry, "web_passwd", password.as_str()),
        Action::AccountSetActive { entry, active } => {
            (*entry, "Active", if *active { "Yes" } else { "No" })
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an account mutation",
            ))
        }
    };
    if entry > 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported account entry",
        ));
    }
    let quoted = format!("'{}'", value.replace('\'', "'\\''"));
    let verify = match action {
        Action::AccountSetActive { .. } => format!(
            "[ \"$(tcapi get Account_Entry{entry} Active 2>/dev/null)\" = {quoted} ] || exit 14; "
        ),
        Action::AccountSetPassword { .. } | Action::AccountsRead | Action::Exec { .. } => {
            String::new()
        }
    };
    Ok(format!("tcapi set Account_Entry{entry} {field} {quoted} >/dev/null 2>&1 || exit 11; tcapi commit Account >/dev/null 2>&1 || exit 12; tcapi save >/dev/null 2>&1 || exit 13; {verify}printf 'ACCOUNT_WRITE_ACCEPTED'"))
}

pub fn run(selection: &Selection, action: &Action) -> io::Result<HlAccountReply> {
    let script = match action {
        Action::AccountsRead => "for i in 0 1 2; do u=$(tcapi get Account_Entry$i username) || exit 1; a=$(tcapi get Account_Entry$i Active) || exit 1; printf '%s\\t%s\\t%s\\n' \"$i\" \"$u\" \"$a\"; done".into(),
        Action::AccountSetPassword { .. } | Action::AccountSetActive { .. } => mutation(action)?,
        Action::Exec { .. } => return Err(io::Error::new(io::ErrorKind::InvalidInput, "not an account action")),
    };
    let mut result = super::command_binding::execute(selection, &script, Duration::from_secs(30))?;
    let output = result.result.output.take();
    let accounts = match action {
        Action::AccountsRead if result.result.success() => {
            Some(parse_metadata(output.as_deref().unwrap_or_default())?)
        }
        Action::AccountsRead
        | Action::AccountSetPassword { .. }
        | Action::AccountSetActive { .. } => None,
        Action::Exec { .. } => None,
    };
    // Typed account results never expose command stdout or credentials.
    result.result.error = result
        .result
        .error
        .map(|_| "account command did not complete; outcome may be unknown".into());
    let write_status = if accounts.is_some() {
        HlWriteStatus::NotApplicable
    } else if result.result.success() {
        HlWriteStatus::AcceptedAndSaved
    } else {
        HlWriteStatus::Unconfirmed
    };
    Ok(HlAccountReply::Accounts {
        schema_version: 1,
        operation: result,
        accounts,
        write_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_metadata_requires_exact_entries_and_never_accepts_a_dump() {
        let rows = parse_metadata("0\tCUAdmin\tYes\n1\tuser\tYes\n2\tuser3\tNo\n").unwrap();
        assert_eq!(rows[2].active, false);
        assert_eq!(rows[0].username, "CUAdmin");
        assert!(parse_metadata("web_passwd=private\n").is_err());
        assert!(parse_metadata("0\tu\tYes\n0\tu\tYes\n2\tu\tNo\n").is_err());
    }

    #[test]
    fn account_password_script_quotes_data_without_reading_or_echoing_passwords() {
        let script = mutation(&super::super::request::Action::AccountSetPassword {
            entry: 1,
            password: "a'$(touch /tmp/never-local)猫".into(),
        })
        .unwrap();
        assert!(script.contains("'a'\\''$(touch /tmp/never-local)猫'"));
        assert!(!script.contains("tcapi get"));
        assert!(!script.contains("username"));
        assert!(!script.contains("display_mask"));
        assert!(!script.contains("isLockState"));
    }

    #[test]
    fn hl202_accounts_real_shell_preserves_one_entry_and_exposes_commit_failure() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};
        let dir = std::env::temp_dir().join(format!("hl202-account-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("tcapi");
        fs::write(&executable, "#!/bin/sh\ncase \"$1\" in set) [ \"$2:$3:$4\" = 'Account_Entry2:Active:No' ];; commit) [ \"$FAIL_COMMIT\" != 1 ];; save) exit 0;; get) printf No;; *) exit 99;; esac\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let script = mutation(&Action::AccountSetActive {
            entry: 2,
            active: false,
        })
        .unwrap();
        let path = format!("{}:/usr/bin:/bin", dir.display());
        let successful = Command::new("sh")
            .args(["-c", &script])
            .env("PATH", &path)
            .env("FAIL_COMMIT", "0")
            .output()
            .unwrap();
        assert_eq!(successful.status.code(), Some(0));
        assert_eq!(successful.stdout, b"ACCOUNT_WRITE_ACCEPTED");
        let rejected = Command::new("sh")
            .args(["-c", &script])
            .env("PATH", &path)
            .env("FAIL_COMMIT", "1")
            .output()
            .unwrap();
        assert_eq!(rejected.status.code(), Some(12));
        assert!(rejected.stdout.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
