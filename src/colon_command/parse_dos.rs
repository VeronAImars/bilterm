//! Parser for `:dos run|setup|doctor|remove|status|reset|snapshot|restore ...` (D2a, D3a). Shares
//! the flag grammar with `bilterm dos ...` (image verbs are validated by the CLI parser itself).
//!
//! The parser is available in every build; execution without the `dos` feature reports that the
//! feature is missing (see `execute_wm`).

use super::ColonCommand;

pub const DOS_USAGE: &str = "Usage: :dos run [--name N | --image FILE] [--drive-c DIR] [--backend qemu-curses] | setup [--yes] [--full] [--entry ID]... | doctor | status | remove [--all] [--yes] | reset [--name N] [--full] [--yes] | snapshot [list|rm] [--name N] [--tag T] | restore --tag T [--name N] [--yes]";

/// Parses `:dos run ...` into `Action::DosRun`; `None` when `head` is not `dos`.
pub fn parse_dos_command(head: &str, parts: &[&str]) -> Option<Result<ColonCommand, String>> {
    if head != "dos" {
        return None;
    }
    let sub = parts.get(1).copied().unwrap_or("");
    let rest: Vec<String> = parts.iter().skip(2).map(|s| (*s).to_string()).collect();
    let bad = || Some(Err(DOS_USAGE.to_string()));
    match sub {
        "run" => {}
        "doctor" | "status" if rest.is_empty() => {
            return Some(Ok(if sub == "doctor" { ColonCommand::DosDoctor } else { ColonCommand::DosStatus }));
        }
        "setup" => {
            let (mut yes, mut full, mut entries, mut it) = (false, false, Vec::new(), rest.iter());
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--yes" | "-y" => yes = true,
                    "--full" => full = true,
                    "--entry" => match it.next() {
                        Some(v) => entries.push(v.clone()),
                        None => return bad(),
                    },
                    _ => return bad(),
                }
            }
            return Some(Ok(ColonCommand::DosSetup { yes, full, entries }));
        }
        "remove" => {
            let (mut all, mut yes) = (false, false);
            for a in &rest {
                match a.as_str() {
                    "--all" => all = true,
                    "--yes" | "-y" => yes = true,
                    _ => return bad(),
                }
            }
            return Some(Ok(ColonCommand::DosRemove { all, yes }));
        }
        "reset" | "snapshot" | "restore" => {
            #[cfg(feature = "dos")]
            if let Err(e) = crate::dos::ops::parse_image(sub, &rest) {
                return Some(Err(e.trim_end().to_string()));
            }
            return Some(Ok(match sub {
                "reset" => ColonCommand::DosReset { args: rest },
                "snapshot" => ColonCommand::DosSnapshot { args: rest },
                _ => ColonCommand::DosRestore { args: rest },
            }));
        }
        _ => return bad(),
    }
    let (mut name, mut image, mut drive_c, mut backend) = (None, None, None, None);
    let mut it = parts[2..].iter();
    while let Some(flag) = it.next() {
        let slot = match *flag {
            "--name" => &mut name,
            "--image" => &mut image,
            "--drive-c" => &mut drive_c,
            "--backend" => &mut backend,
            _ => return Some(Err(DOS_USAGE.to_string())),
        };
        match it.next() {
            Some(v) if slot.is_none() => *slot = Some((*v).to_string()),
            _ => return Some(Err(DOS_USAGE.to_string())),
        }
    }
    if name.is_some() && image.is_some() {
        return Some(Err(format!("--name and --image are mutually exclusive. {DOS_USAGE}")));
    }
    Some(Ok(ColonCommand::DosRun { name, image, drive_c, backend }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colon_command::parse_colon_command;

    fn p(s: &str) -> Result<ColonCommand, String> {
        parse_colon_command(s).unwrap()
    }

    #[test]
    fn parses_flags_and_round_trips_colon_form() {
        let a = p(":dos run --name work --drive-c /srv/dos").unwrap();
        assert_eq!(
            a,
            ColonCommand::DosRun {
                name: Some("work".into()),
                image: None,
                drive_c: Some("/srv/dos".into()),
                backend: None
            }
        );
        assert_eq!(p(&a.colon_form_or_summary()).unwrap(), a);
    }

    #[test]
    fn parses_lifecycle_verbs_and_round_trips() {
        for t in [
            ":dos setup --yes --full --entry fd14",
            ":dos doctor",
            ":dos status",
            ":dos remove --all --yes",
            ":dos reset --name a",
            ":dos snapshot",
            ":dos snapshot list",
            ":dos restore --tag t1 --yes",
        ] {
            let a = p(t).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert_eq!(p(&a.colon_form_or_summary()).unwrap(), a, "{t}");
        }
    }

    #[cfg(feature = "dos")]
    #[test]
    fn image_verbs_are_validated_by_the_cli_grammar() {
        assert!(p(":dos restore").is_err());
        assert!(p(":dos reset --bogus").is_err());
    }

    #[test]
    fn rejects_bad_forms() {
        for bad in [":dos", ":dos stop", ":dos doctor x", ":dos setup --entry", ":dos remove --x", ":dos run --bogus x", ":dos run --name", ":dos run --name a --name b", ":dos run --name a --image b"] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
