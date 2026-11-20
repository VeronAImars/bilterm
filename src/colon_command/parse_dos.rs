//! Parser for `:dos run ...` (PR D2a). Shares the flag grammar with `bilterm dos run`.
//!
//! The parser is available in every build; execution without the `dos` feature reports that the
//! feature is missing (see `execute_wm`).

use super::ColonCommand;

pub const DOS_USAGE: &str =
    "Usage: :dos run [--name N | --image FILE] [--drive-c DIR] [--backend qemu-curses]";

/// Parses `:dos run ...` into `Action::DosRun`; `None` when `head` is not `dos`.
pub fn parse_dos_command(head: &str, parts: &[&str]) -> Option<Result<ColonCommand, String>> {
    if head != "dos" {
        return None;
    }
    if parts.get(1).copied() != Some("run") {
        return Some(Err(DOS_USAGE.to_string()));
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
    fn rejects_bad_forms() {
        for bad in [":dos", ":dos stop", ":dos run --bogus x", ":dos run --name", ":dos run --name a --name b", ":dos run --name a --image b"] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
