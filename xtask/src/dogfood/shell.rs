//! The shell arm: the calibration's sessions played once more with the levers
//! as they ship and the client's own read and search tools withheld, so every
//! read goes through its shell.
//!
//! The field's traffic reads mostly through the shell, while every scripted
//! session otherwise reads through the read tool — so without this arm the
//! calibration never exercises the shape real sessions have. It is a module of
//! its own because it sits *beside* the two-arm calibration rather than inside
//! it: the two-arm table, document and layout are what they were, and this
//! arm is played, printed and written only when `--bypass-shell` asks for it.

use std::path::{Path, PathBuf};

use crate::dogfood::{Client, ClientRun, Options, SessionOutcome, ShellArm, Status};
use crate::dogfood_compare::{
    add_opt, compare_json, pad, render_compare_table, session_numbers, Arm, CompareRun, ARMS,
    SUMMED,
};

/// One client's shell arm, beside the on arm it is read against.
#[derive(Clone, Debug)]
pub(crate) struct ShellRun {
    /// The client both arms were played through.
    pub(crate) client: Client,
    /// The arm as the levers ship, read through the client's own tools.
    pub(crate) on: ClientRun,
    /// The same levers, read only through the client's shell.
    pub(crate) shell: ClientRun,
    /// Where the client wrote the shell arm's transcripts, so the offline
    /// replay can census them; `None` when there is no home to derive it from.
    pub(crate) transcripts: Option<PathBuf>,
}

/// What one arm booked and was charged over a session or a sum of them — the
/// figures [`crate::dogfood_compare::CompareRow`] carries, per arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArmFigures {
    /// Read tokens the family booked.
    pub(crate) read: i64,
    /// What the family booked as avoided.
    pub(crate) avoided: i64,
    /// What the family booked as returned.
    pub(crate) returned: i64,
    /// What the client itself said the arm cost it.
    pub(crate) reported: Option<u64>,
}

/// One line of the shell reading: a session, or a client's sum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellRow {
    /// The client's name.
    pub(crate) client: String,
    /// The session's name, or the word for a summed row.
    pub(crate) session: String,
    /// The on arm's figures.
    pub(crate) on: ArmFigures,
    /// The shell arm's figures.
    pub(crate) shell: ArmFigures,
    /// Whether both arms of this row ran to completion.
    pub(crate) both_completed: bool,
}

impl ArmFigures {
    /// The identity a sum starts from: an absent client figure poisons the sum
    /// rather than counting as zero, so the seed is zero, not absent.
    const ZERO: Self = Self {
        read: 0,
        avoided: 0,
        returned: 0,
        reported: Some(0),
    };

    /// One session's figures, or nothing booked for a session the arm never
    /// played.
    fn of(session: Option<&SessionOutcome>) -> Self {
        let Some(session) = session else {
            return Self::default();
        };
        Self {
            read: session.deltas.read_tokens,
            avoided: session.deltas.tokens_avoided,
            returned: session.deltas.tokens_returned,
            reported: session.client_reported,
        }
    }

    /// Both sets added, where an absent client figure stays absent.
    fn plus(self, other: Self) -> Self {
        Self {
            read: self.read + other.read,
            avoided: self.avoided + other.avoided,
            returned: self.returned + other.returned,
            reported: add_opt(self.reported, other.reported),
        }
    }
}

/// The arms one client plays under these options, in the order it plays them.
///
/// `None` is the single-arm run. The shell arm follows the calibration's two
/// only where it was asked for and the client plays it; see
/// [`Client::shell_arm`] for why a client does not.
pub(crate) fn arms_for(opts: &Options, client: Client) -> Vec<Option<Arm>> {
    if !opts.compare {
        return vec![None];
    }
    let mut arms: Vec<Option<Arm>> = ARMS.iter().copied().map(Some).collect();
    if opts.bypass_shell && client.shell_arm() == ShellArm::Plays {
        arms.push(Some(Arm::Shell));
    }
    arms
}

/// Where Claude Code writes the transcripts of a session run in `scratch`.
///
/// The client names a project's directory after its working directory with
/// every character that is not an ASCII letter or digit turned into `-`, so
/// `/a/b.c` lands in `<home>/.claude/projects/-a-b-c`. The harness only names
/// the directory; it never reads or copies what the client wrote there.
pub(crate) fn claude_code_transcript_dir(home: &Path, scratch: &Path) -> PathBuf {
    let slug: String = scratch
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    home.join(".claude").join("projects").join(slug)
}

/// The calibration's table and document, with the shell reading beside them
/// when the run played one.
///
/// Without a shell reading both are the two-arm calibration's own, byte for
/// byte: the shell block is appended and the shell key added, and nothing that
/// was there is rewritten.
pub(crate) fn calibration(
    pairs: &[CompareRun],
    shells: Option<&[ShellRun]>,
    stamp: &str,
) -> (String, serde_json::Value) {
    let mut table = render_compare_table(pairs);
    let mut json = compare_json(pairs, stamp);
    let Some(shells) = shells else {
        return (table, json);
    };
    table.push('\n');
    table.push_str(&render_shell_block(shells));
    if let Some(document) = json.as_object_mut() {
        document.insert("shell".to_owned(), shell_json(shells));
    }
    (table, json)
}

/// One row per session the two arms played, then the client's sum.
///
/// Sessions are paired by ordinal, as the calibration pairs them.
pub(crate) fn shell_rows_for(run: &ShellRun) -> Vec<ShellRow> {
    let completed = |s: Option<&SessionOutcome>| s.is_some_and(|s| s.status == Status::Completed);
    let mut rows: Vec<ShellRow> = session_numbers(&run.on, &run.shell)
        .into_iter()
        .map(|number| {
            let on = find_session(&run.on, number);
            let shell = find_session(&run.shell, number);
            ShellRow {
                client: run.client.slug().to_owned(),
                session: on
                    .or(shell)
                    .map_or_else(|| format!("{number:02}"), |s| s.name.clone()),
                on: ArmFigures::of(on),
                shell: ArmFigures::of(shell),
                both_completed: completed(on) && completed(shell),
            }
        })
        .collect();
    let total = ShellRow {
        client: run.client.slug().to_owned(),
        session: SUMMED.to_owned(),
        on: rows.iter().fold(ArmFigures::ZERO, |acc, r| acc.plus(r.on)),
        shell: rows
            .iter()
            .fold(ArmFigures::ZERO, |acc, r| acc.plus(r.shell)),
        both_completed: rows.iter().all(|r| r.both_completed),
    };
    rows.push(total);
    rows
}

/// The session an arm played under this ordinal, if it played one.
fn find_session(arm: &ClientRun, number: u32) -> Option<&SessionOutcome> {
    arm.sessions.iter().find(|s| s.number == number)
}

/// The shell reading as the table prints it: the on and shell arms side by
/// side per session and per client, then where each client's transcripts are
/// and the line that censuses them.
pub(crate) fn render_shell_block(shells: &[ShellRun]) -> String {
    let mut out = String::new();
    out.push_str("shell arm — the levers as they ship, read only through the client's shell\n");
    out.push_str(&format!(
        "  {}{}{:>9}{:>12}{:>12}{:>15}{:>13}{:>16}{:>11}{:>14}\n",
        pad("client", 14),
        pad("session", 26),
        "read_on",
        "read_shell",
        "avoided_on",
        "avoided_shell",
        "returned_on",
        "returned_shell",
        "client_on",
        "client_shell"
    ));
    let mut marked = false;
    for row in shells.iter().flat_map(shell_rows_for) {
        marked |= !row.both_completed;
        out.push_str(&shell_row_line(&row));
    }
    if shells.is_empty() {
        out.push_str("  (no client played the shell arm)\n");
    }
    if marked {
        out.push_str("  ! one arm of this row did not complete — it is not a measurement\n");
    }
    for run in shells {
        out.push_str(&census_lines(run));
    }
    out
}

/// One row of the shell table.
fn shell_row_line(row: &ShellRow) -> String {
    let mark = if row.both_completed { "" } else { " !" };
    let reported = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
    format!(
        "  {}{}{:>9}{:>12}{:>12}{:>15}{:>13}{:>16}{:>11}{:>14}\n",
        pad(&row.client, 14),
        pad(&format!("{}{mark}", row.session), 26),
        row.on.read,
        row.shell.read,
        row.on.avoided,
        row.shell.avoided,
        row.on.returned,
        row.shell.returned,
        reported(row.on.reported),
        reported(row.shell.reported),
    )
}

/// Where one client's shell transcripts are, and the census that reads them.
fn census_lines(run: &ShellRun) -> String {
    let Some(dir) = &run.transcripts else {
        return format!(
            "  {} transcripts: - (no home directory to derive them from)\n",
            run.client.slug()
        );
    };
    format!(
        "  {} transcripts: {}\n    census: cargo run -p xtask -- replay-savings --transcripts {} --repo {}\n",
        run.client.slug(),
        dir.display(),
        dir.display(),
        run.shell.scratch.display()
    )
}

/// The shell reading as the document carries it, under its own key.
pub(crate) fn shell_json(shells: &[ShellRun]) -> serde_json::Value {
    let arm = |run: &ClientRun| {
        serde_json::json!({
            "scratch": run.scratch.display().to_string(),
            "recall_db": run.recall_db,
            "projection_db": run.projection_db,
            "sessions_completed": run.completed(),
            "sessions_played": run.sessions.len(),
        })
    };
    let row = |r: &ShellRow| {
        serde_json::json!({
            "client": r.client,
            "session": r.session,
            "read_on": r.on.read,
            "read_shell": r.shell.read,
            "booked_avoided_on": r.on.avoided,
            "booked_avoided_shell": r.shell.avoided,
            "booked_returned_on": r.on.returned,
            "booked_returned_shell": r.shell.returned,
            "client_reported_on": r.on.reported,
            "client_reported_shell": r.shell.reported,
            "both_arms_completed": r.both_completed,
        })
    };
    serde_json::Value::Array(
        shells
            .iter()
            .map(|run| {
                serde_json::json!({
                    "client": run.client.slug(),
                    "on": arm(&run.on),
                    "shell": arm(&run.shell),
                    "transcripts": run.transcripts.as_ref().map(|d| d.display().to_string()),
                    "rows": shell_rows_for(run).iter().map(row).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dogfood::{RetryCounts, SavingsDeltas, SavingsSample};

    /// One session of one arm: what it booked, what the client said it cost,
    /// and whether it finished.
    fn session(number: u32, read: i64, avoided: i64, reported: Option<u64>) -> SessionOutcome {
        SessionOutcome {
            number,
            name: format!("{number:02}-a-session"),
            status: Status::Completed,
            exit_code: Some(0),
            seconds: 9,
            savings: Some(SavingsSample::default()),
            deltas: SavingsDeltas {
                read_tokens: read,
                tokens_avoided: avoided,
                tokens_prevented: None,
                tokens_returned: avoided / 4,
            },
            client_reported: reported,
            ledger_lines: 3,
            retries: RetryCounts::default(),
            error: None,
        }
    }

    /// One arm of one client, in a checkout of its own.
    fn arm_run(arm: Arm, sessions: Vec<SessionOutcome>) -> ClientRun {
        let root = PathBuf::from("/scratch/claude-code").join(arm.label());
        ClientRun {
            client: Client::ClaudeCode,
            scratch: root.join("repo"),
            recall_db: root.join("recall.db").display().to_string(),
            projection_db: root.join("projection.db").display().to_string(),
            sessions,
            final_savings: None,
            kinds: Vec::new(),
        }
    }

    /// The calibration of the fixture: two sessions, both arms complete.
    fn pair() -> CompareRun {
        CompareRun {
            client: Client::ClaudeCode,
            off: arm_run(
                Arm::Off,
                vec![
                    session(1, 12_000, 0, Some(41_000)),
                    session(2, 5_000, 0, None),
                ],
            ),
            on: arm_run(
                Arm::On,
                vec![
                    session(1, 9_000, 2_800, Some(38_000)),
                    session(2, 4_000, 900, None),
                ],
            ),
        }
    }

    /// The shell arm of the fixture, beside the calibration's on arm; its
    /// second session timed out.
    fn shell_run() -> ShellRun {
        let mut timed_out = session(2, 1_000, 100, None);
        timed_out.status = Status::TimedOut;
        ShellRun {
            client: Client::ClaudeCode,
            on: pair().on,
            shell: arm_run(
                Arm::Shell,
                vec![session(1, 3_000, 1_200, Some(52_000)), timed_out],
            ),
            transcripts: Some(PathBuf::from(
                "/home/dev/.claude/projects/-scratch-claude-code-shell-repo",
            )),
        }
    }

    /// The options a line with these two flags parses to.
    fn options(clients: Vec<Client>, compare: bool, bypass_shell: bool) -> Options {
        Options {
            clients,
            sessions: 2,
            out: None,
            keep: false,
            compare,
            bypass_shell,
        }
    }

    /// The two-arm calibration, as the base renders it for the fixture.
    const TWO_ARM_TABLE: &str = r"calibration — the same scripted sessions with every read lever off, and as they ship
  client        session                      read_off    read_on   measured    avoided  prevented  calibration  client_off   client_on
  claude-code   01-a-session                    12000       9000       3000       2800          -       93.33%       41000       38000
  claude-code   02-a-session                     5000       4000       1000        900          -       90.00%           -           -
  claude-code   (all sessions)                  17000      13000       4000       3700          -       92.50%           -           -
  overall       (all sessions)                  17000      13000       4000       3700          -       92.50%           -           -

  the two arms are separate, non-deterministic model runs against separate
  families: a difference between them carries the levers and the day's tool
  choices together. A negative measured removal means the arm with the levers
  on read more, and is reported as it stands, never clamped.
";

    /// Without the shell arm the calibration's table and document are the
    /// two-arm calibration's own, byte for byte, and the table is the one
    /// pinned here.
    #[test]
    fn the_two_arm_calibration_is_unchanged_without_the_shell_arm() {
        let pairs = [pair()];
        let (table, json) = calibration(&pairs, None, "20260909-120000");
        assert_eq!(table, render_compare_table(&pairs));
        assert_eq!(table, TWO_ARM_TABLE, "\n{table}");
        assert_eq!(json, compare_json(&pairs, "20260909-120000"));
        assert!(json.get("shell").is_none());
    }

    /// With the shell arm the two-arm table is still the table's first part,
    /// every key the document had keeps its value, and the shell reading is
    /// added under a key of its own.
    #[test]
    fn the_shell_reading_is_added_beside_the_calibration() {
        let pairs = [pair()];
        let (base_table, base_json) = calibration(&pairs, None, "s");
        let (table, json) = calibration(&pairs, Some(&[shell_run()]), "s");
        assert!(table.starts_with(&base_table), "{table}");
        assert!(table.contains("shell arm — "), "{table}");
        let Some(base) = base_json.as_object() else {
            panic!("the calibration is one document");
        };
        for (key, value) in base {
            assert_eq!(&json[key], value, "{key} keeps its value");
        }
        let shell = &json["shell"][0];
        assert_eq!(shell["client"], "claude-code");
        assert_eq!(shell["shell"]["scratch"], "/scratch/claude-code/shell/repo");
        assert_eq!(shell["rows"][0]["read_on"], 9_000);
        assert_eq!(shell["rows"][0]["read_shell"], 3_000);
        assert_eq!(shell["rows"][0]["booked_avoided_shell"], 1_200);
        assert_eq!(shell["rows"][0]["booked_returned_shell"], 300);
        assert_eq!(shell["rows"][0]["client_reported_shell"], 52_000);
        assert_eq!(shell["rows"][1]["both_arms_completed"], false);
        assert_eq!(shell["rows"][2]["session"], SUMMED);
        assert!(
            shell["rows"][2]["client_reported_on"].is_null(),
            "one silent session"
        );
        assert_eq!(
            shell["transcripts"],
            "/home/dev/.claude/projects/-scratch-claude-code-shell-repo"
        );
    }

    /// The shell block over the fixture, pinned: the two arms side by side
    /// per session and per client, the mark on the row one arm did not
    /// finish, and where the transcripts are with the census that reads them.
    #[test]
    fn the_shell_block_prints_both_arms_side_by_side() {
        let block = render_shell_block(&[shell_run()]);
        assert_eq!(block, SHELL_BLOCK, "\n{block}");
        assert_eq!(
            render_shell_block(&[]),
            format!("{SHELL_HEADER}  (no client played the shell arm)\n"),
        );
    }

    /// The shell block the fixture renders.
    const SHELL_BLOCK: &str = r"shell arm — the levers as they ship, read only through the client's shell
  client        session                     read_on  read_shell  avoided_on  avoided_shell  returned_on  returned_shell  client_on  client_shell
  claude-code   01-a-session                   9000        3000        2800           1200          700             300      38000         52000
  claude-code   02-a-session !                 4000        1000         900            100          225              25          -             -
  claude-code   (all sessions) !              13000        4000        3700           1300          925             325          -             -
  ! one arm of this row did not complete — it is not a measurement
  claude-code transcripts: /home/dev/.claude/projects/-scratch-claude-code-shell-repo
    census: cargo run -p xtask -- replay-savings --transcripts /home/dev/.claude/projects/-scratch-claude-code-shell-repo --repo /scratch/claude-code/shell/repo
";
    /// The two lines every shell block opens with.
    const SHELL_HEADER: &str = r"shell arm — the levers as they ship, read only through the client's shell
  client        session                     read_on  read_shell  avoided_on  avoided_shell  returned_on  returned_shell  client_on  client_shell
";

    /// The arm set follows the options: one arm alone, two for the
    /// calibration, and a third only where it was asked for and the client
    /// plays it.
    #[test]
    fn the_arms_played_follow_the_options_and_the_client() {
        let single = options(vec![Client::ClaudeCode], false, false);
        assert_eq!(arms_for(&single, Client::ClaudeCode), vec![None]);
        let two = options(vec![Client::ClaudeCode], true, false);
        assert_eq!(
            arms_for(&two, Client::ClaudeCode),
            vec![Some(Arm::Off), Some(Arm::On)]
        );
        let three = options(vec![Client::ClaudeCode], true, true);
        assert_eq!(
            arms_for(&three, Client::ClaudeCode),
            vec![Some(Arm::Off), Some(Arm::On), Some(Arm::Shell)]
        );
        for client in [Client::Codex, Client::Antigravity] {
            assert_eq!(
                arms_for(&three, client),
                vec![Some(Arm::Off), Some(Arm::On)],
                "{} plays no shell arm",
                client.slug()
            );
        }
    }

    /// Claude Code's project directory is the working directory with every
    /// character that is not a letter or digit turned into a dash.
    #[test]
    fn the_transcript_directory_is_derived_from_the_scratch_path() {
        let home = Path::new("/Users/dev");
        assert_eq!(
            claude_code_transcript_dir(
                home,
                Path::new("/Users/dev/rift-dogfood/20260909-120000/claude-code/shell/repo")
            ),
            PathBuf::from(
                "/Users/dev/.claude/projects/-Users-dev-rift-dogfood-20260909-120000-claude-code-shell-repo"
            )
        );
        assert_eq!(
            claude_code_transcript_dir(home, Path::new("/tmp/a.b_c/repo")),
            PathBuf::from("/Users/dev/.claude/projects/-tmp-a-b-c-repo")
        );
    }

    /// The labels the layout, the table and the document use, and the
    /// progress wording the two calibration arms have always printed.
    #[test]
    fn the_shell_arm_has_its_own_directory_and_heading() {
        assert_eq!(Arm::Shell.label(), "shell");
        assert_eq!(Arm::Off.heading(), " (off levers)");
        assert_eq!(Arm::On.heading(), " (on levers)");
        assert_eq!(Arm::Shell.heading(), " (on levers, shell reads only)");
        assert_eq!(ARMS, [Arm::Off, Arm::On], "the calibration stays two arms");
    }
}
