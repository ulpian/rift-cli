//! The command line: what one invocation was asked for.

use std::path::PathBuf;

use crate::dogfood::Client;

/// How many minutes one scripted session may run before the box fires.
pub(super) const SESSION_MINUTES: u64 = 10;

/// A read lever the harness can write at `enforce` for a run, named by its
/// `[interception]` key.
///
/// Only the levers that ship at `observe` are here: `mode` is the read lever
/// itself and already enforces, and an arm that switched it would be the
/// calibration's off arm, not an enforce arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lever {
    /// `first_contact`: a first read of a large file answered with its outline.
    FirstContact,
    /// `narrow`: the held part of a partly held read declined for the rest.
    Narrow,
    /// `inherit_by_name`: a subagent's lines of a path its prompt named.
    InheritByName,
    /// `decline_family_held`: a read another live window holds.
    DeclineFamilyHeld,
    /// `recognise_by_output`: lines a shell result was matched to a file as carrying.
    RecogniseByOutput,
}

impl Lever {
    /// Every lever, in the order the usage line names them.
    pub const ALL: [Self; 5] = [
        Self::FirstContact,
        Self::Narrow,
        Self::InheritByName,
        Self::DeclineFamilyHeld,
        Self::RecogniseByOutput,
    ];

    /// The `[interception]` key this lever is written under.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::FirstContact => "first_contact",
            Self::Narrow => "narrow",
            Self::InheritByName => "inherit_by_name",
            Self::DeclineFamilyHeld => "decline_family_held",
            Self::RecogniseByOutput => "recognise_by_output",
        }
    }

    /// Parse one `--enforce` value: lever keys, comma-separated.
    ///
    /// # Errors
    /// An empty list, or a name that is not a lever's key — spelled back with
    /// the keys that are.
    pub fn parse_list(value: &str) -> Result<Vec<Self>, String> {
        let names: Vec<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        if names.is_empty() {
            return Err("dogfood: --enforce names no lever".to_owned());
        }
        names
            .into_iter()
            .map(|name| {
                Self::ALL
                    .into_iter()
                    .find(|lever| lever.key() == name)
                    .ok_or_else(|| {
                        format!(
                            "dogfood: unknown lever `{name}` — one of {}",
                            Self::ALL.map(Self::key).join(", ")
                        )
                    })
            })
            .collect()
    }
}

/// What one invocation was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The clients to play, in table order.
    pub clients: Vec<Client>,
    /// How many of the scripted sessions to play, from the first.
    pub sessions: usize,
    /// Where the scratch checkouts and logs go. `None` means the default
    /// under the home directory, stamped with the run's own UTC instant.
    pub out: Option<PathBuf>,
    /// Keep the scratch checkouts (and their registry rows) afterwards.
    pub keep: bool,
    /// Play every session twice — once with every read lever off, once as
    /// they ship — and print the calibration between the two arms. It doubles
    /// what the run spends.
    pub compare: bool,
    /// With `compare`, play a third arm per client that reads only through the
    /// client's shell, beside the on arm. Most of the field's traffic reads
    /// that way and no scripted session otherwise does.
    pub bypass_shell: bool,
    /// The levers written at `enforce` in the scratch checkouts the sessions
    /// play in, in the order the line named them. Empty is today's run: the
    /// policy as it ships, and nothing written.
    pub enforce: Vec<Lever>,
}

/// Parse the argv after the `dogfood` token.
///
/// # Errors
/// A missing or unknown `--client`, a flag without its value, an
/// unrecognised flag, a lever `--enforce` does not know, or `--bypass-shell`
/// without `--compare`.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    const USAGE: &str = "usage: xtask dogfood --client <claude-code|codex|antigravity|all> \
                         [--sessions N] [--out DIR] [--keep] [--compare [--bypass-shell]] \
                         [--enforce <lever>[,<lever>…]]";
    let mut clients = None;
    let mut sessions = 10usize;
    let mut out = None;
    let mut keep = false;
    let mut compare = false;
    let mut bypass_shell = false;
    let mut enforce = Vec::new();
    let mut at = 0usize;
    while let Some(arg) = args.get(at) {
        let value = |name: &str| -> Result<String, String> {
            args.get(at + 1)
                .cloned()
                .ok_or_else(|| format!("dogfood: {name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--client" => {
                clients = Some(Client::parse(&value("--client")?)?);
                at += 2;
            }
            "--sessions" => {
                sessions = value("--sessions")?
                    .parse()
                    .map_err(|_| format!("dogfood: --sessions wants a number\n{USAGE}"))?;
                at += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(value("--out")?));
                at += 2;
            }
            "--keep" => {
                keep = true;
                at += 1;
            }
            "--compare" => {
                compare = true;
                at += 1;
            }
            "--bypass-shell" => {
                bypass_shell = true;
                at += 1;
            }
            "--enforce" => {
                enforce.extend(Lever::parse_list(&value("--enforce")?)?);
                at += 2;
            }
            other => return Err(format!("dogfood: unknown argument `{other}`\n{USAGE}")),
        }
    }
    let clients = clients.ok_or_else(|| format!("dogfood: --client is required\n{USAGE}"))?;
    if sessions == 0 {
        return Err(format!("dogfood: --sessions must be at least 1\n{USAGE}"));
    }
    // The shell arm is read beside the on arm, so without the calibration
    // there is nothing to read it against.
    if bypass_shell && !compare {
        return Err(format!(
            "dogfood: --bypass-shell is only valid together with --compare\n{USAGE}"
        ));
    }
    Ok(Options {
        clients,
        sessions,
        out,
        keep,
        compare,
        bypass_shell,
        enforce,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::ALL_CLIENTS;

    /// Every flag, and each way the line can be wrong.
    #[test]
    fn the_arguments_parse() {
        let args: Vec<String> = [
            "--client",
            "all",
            "--sessions",
            "3",
            "--out",
            "/o",
            "--keep",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let opts = parse_args(&args).unwrap();
        assert_eq!(opts.clients, ALL_CLIENTS.to_vec());
        assert_eq!(opts.sessions, 3);
        assert_eq!(opts.out, Some(PathBuf::from("/o")));
        assert!(opts.keep);
        assert!(!opts.compare, "one arm unless the line asks for two");
        let both = parse_args(&[
            "--client".to_owned(),
            "codex".to_owned(),
            "--compare".to_owned(),
        ])
        .unwrap();
        assert!(both.compare);
        assert!(!both.keep, "the two flags are independent");
        let one = parse_args(&["--client".to_owned(), "codex".to_owned()]).unwrap();
        assert_eq!(one.clients, vec![Client::Codex]);
        assert_eq!(one.sessions, 10);
        assert!(!one.keep);
        assert!(parse_args(&[]).is_err(), "--client is required");
        assert!(
            parse_args(&["--compare".to_owned()]).is_err(),
            "still no client"
        );
        assert!(parse_args(&["--client".to_owned(), "vim".to_owned()]).is_err());
        assert!(parse_args(&["--client".to_owned()]).is_err(), "no value");
        assert!(parse_args(&["--wat".to_owned()]).is_err());
        assert!(parse_args(&[
            "--client".to_owned(),
            "codex".to_owned(),
            "--sessions".to_owned(),
            "0".to_owned()
        ])
        .is_err());
    }

    /// `--enforce` names the levers to write at `enforce`, by their policy
    /// keys, comma-separated or repeated; a name that is not a lever is
    /// refused with the list of those that are; and without the flag no
    /// lever is named — today's run.
    #[test]
    fn the_enforce_flag_names_levers_and_refuses_unknown_ones() {
        let line =
            |words: &[&str]| -> Vec<String> { words.iter().map(|s| (*s).to_owned()).collect() };
        let none = parse_args(&line(&["--client", "codex"])).unwrap();
        assert!(
            none.enforce.is_empty(),
            "nothing is written without the flag"
        );
        let two = parse_args(&line(&[
            "--client",
            "codex",
            "--enforce",
            "first_contact,narrow",
        ]))
        .unwrap();
        assert_eq!(two.enforce, vec![Lever::FirstContact, Lever::Narrow]);
        let repeated = parse_args(&line(&[
            "--enforce",
            "inherit_by_name",
            "--client",
            "all",
            "--enforce",
            "decline_family_held,recognise_by_output",
        ]))
        .unwrap();
        assert_eq!(
            repeated.enforce,
            vec![
                Lever::InheritByName,
                Lever::DeclineFamilyHeld,
                Lever::RecogniseByOutput
            ]
        );
        assert_eq!(
            Lever::ALL.iter().map(|l| l.key()).collect::<Vec<_>>(),
            [
                "first_contact",
                "narrow",
                "inherit_by_name",
                "decline_family_held",
                "recognise_by_output"
            ],
            "the keys are the policy file's"
        );
        let err = parse_args(&line(&["--client", "codex", "--enforce", "mode"])).unwrap_err();
        assert!(err.contains("unknown lever `mode`"), "{err}");
        assert!(err.contains("first_contact"), "{err}");
        assert!(
            parse_args(&line(&["--client", "codex", "--enforce"])).is_err(),
            "no value"
        );
        assert!(
            parse_args(&line(&["--client", "codex", "--enforce", ""])).is_err(),
            "an empty list names nothing"
        );
    }

    /// The shell arm is asked for only beside the calibration, and the line
    /// that asks for it alone is refused with the usage it got wrong.
    #[test]
    fn bypass_shell_parses_only_with_compare() {
        let line =
            |words: &[&str]| -> Vec<String> { words.iter().map(|s| (*s).to_owned()).collect() };
        let three = parse_args(&line(&["--client", "all", "--compare", "--bypass-shell"])).unwrap();
        assert!(three.compare && three.bypass_shell);
        let either_order =
            parse_args(&line(&["--bypass-shell", "--client", "codex", "--compare"])).unwrap();
        assert!(either_order.bypass_shell);
        let two = parse_args(&line(&["--client", "codex", "--compare"])).unwrap();
        assert!(
            !two.bypass_shell,
            "two arms unless the line asks for the third"
        );
        let err = parse_args(&line(&["--client", "codex", "--bypass-shell"])).unwrap_err();
        assert!(err.contains("only valid together with --compare"), "{err}");
        assert!(err.contains("usage: xtask dogfood"), "{err}");
    }
}
