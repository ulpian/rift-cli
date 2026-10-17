//! How a run happens: one session played, one client's set, and the whole
//! command from the refusal under CI to the swept checkouts.

use std::path::{Path, PathBuf};

use crate::dogfood::measure::{
    parse_kinds, parse_savings, retries_after_deny, unavailability, LeverKind, RetryCounts,
    SavingsDeltas, SavingsSample,
};
use crate::dogfood::options::{Options, SESSION_MINUTES};
use crate::dogfood::process::{capture, capture_with_path, must, release_binary, resolve_program};
use crate::dogfood::report::{
    fmt_share, render_table, report_json, utc_stamp, ClientRun, SessionOutcome, Status,
};
use crate::dogfood::scratch::{
    codex_config, ledger_lines, prepare, read_savings, tidy, ArmOf, Untouched,
};
use crate::dogfood::sessions::{load_sessions, Session, SESSIONS_DIR};
use crate::dogfood::shell::{arms_for, calibration, claude_code_transcript_dir, ShellRun};
use crate::dogfood::{Client, Lever, ShellArm};
use crate::dogfood_compare::{Arm, CompareRun};

/// What one client's sessions are played against.
///
/// The client, the two binaries and the two directories travel together
/// through every step of one client's run, so they are carried as one context
/// rather than repeated as five parameters at each of them.
struct Playing<'a> {
    /// The client being driven.
    client: Client,
    /// The arm the checkout plays, which decides how the client is driven.
    arm: Option<Arm>,
    /// Its executable, as the machine resolved it.
    program: PathBuf,
    /// The release binary the scratch checkout's hooks were pointed at.
    rift: &'a Path,
    /// The scratch checkout the sessions play in.
    scratch: PathBuf,
    /// Where each session's own streams are written.
    log_dir: PathBuf,
}

impl Playing<'_> {
    /// Every session in order, each carrying the move it alone made.
    ///
    /// The reading a session is measured against is the one taken before it,
    /// so each session carries what it alone moved rather than the family's
    /// running total. The first of them is the end of the preparation, so
    /// session one carries no part of the priming.
    fn play_all(&self, sessions: &[Session], baseline: &SavingsSample) -> Vec<SessionOutcome> {
        let mut played = Vec::with_capacity(sessions.len());
        let mut before = baseline.clone();
        for session in sessions {
            let mut outcome = self.play(session);
            outcome.ledger_lines = ledger_lines(&self.scratch);
            if let Some(after) = self.savings_after(&outcome.name) {
                outcome.deltas = SavingsDeltas::between(&before, &after);
                before = after.clone();
                outcome.savings = Some(after);
            }
            println!(
                "  {} {} in {}s — share {}",
                outcome.name,
                outcome.status.label(),
                outcome.seconds,
                fmt_share(outcome.savings.as_ref().and_then(|s| s.share_pct))
            );
            played.push(outcome);
        }
        played
    }

    /// Play one session, time-boxed, and read what its streams expose.
    fn play(&self, session: &Session) -> SessionOutcome {
        let started = std::time::Instant::now();
        let result = capture_with_path(
            Path::new("/usr/bin/perl"),
            &self.time_boxed_argv(session),
            &self.scratch,
            &self.leading_path(),
        );
        let mut outcome = SessionOutcome {
            number: session.number,
            name: session.name.clone(),
            status: Status::Failed,
            exit_code: None,
            seconds: started.elapsed().as_secs(),
            savings: None,
            deltas: SavingsDeltas::default(),
            client_reported: None,
            ledger_lines: 0,
            retries: RetryCounts::default(),
            error: None,
        };
        let (code, stdout, stderr) = match result {
            Ok(v) => v,
            Err(e) => {
                outcome.status = Status::Unavailable;
                outcome.error = Some(e);
                return outcome;
            }
        };
        let stem = &session.name;
        let _ = std::fs::write(self.log_dir.join(format!("{stem}.stream.jsonl")), &stdout);
        let _ = std::fs::write(self.log_dir.join(format!("{stem}.err.txt")), &stderr);
        let both = format!("{stdout}\n{stderr}");
        outcome.exit_code = code;
        outcome.retries = retries_after_deny(&both);
        outcome.client_reported =
            crate::dogfood_compare::client_reported_tokens(self.client, &stdout);
        outcome.status = status_for(code);
        if let Some(reason) = unavailability(&both) {
            outcome.status = Status::Unavailable;
            outcome.error = Some(reason);
        } else if outcome.status != Status::Completed {
            outcome.error = Some(last_stderr_line(&stderr));
        }
        outcome
    }

    /// The argument vector that plays one session under a time box.
    ///
    /// This machine has no `timeout` binary, so the box is `perl`'s alarm,
    /// which is what the archived measurements used.
    fn time_boxed_argv(&self, session: &Session) -> Vec<String> {
        let mut argv = vec![
            "-e".to_owned(),
            "alarm shift; exec @ARGV".to_owned(),
            (SESSION_MINUTES * 60).to_string(),
            self.program.display().to_string(),
        ];
        argv.extend(
            self.client
                .argv_for(self.arm, &session.prompt, &self.scratch),
        );
        argv
    }

    /// `PATH` with the release binary's own directory in front of it.
    ///
    /// The frames Rift injects name `rift` as a command to run. On a
    /// developer's machine the binary is on PATH; here it is a build artifact,
    /// and a session that cannot run the verb it was pointed at measures a
    /// broken install rather than a lever. So the release binary's own
    /// directory leads PATH for the client and for anything the model runs
    /// inside it.
    fn leading_path(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        match self.rift.parent() {
            Some(dir) => format!("{}:{inherited}", dir.display()),
            None => inherited,
        }
    }

    /// The family's figures after one session, written beside that session's
    /// log. A verb that refused, or output the harness cannot read, leaves the
    /// session without figures rather than with zeros.
    fn savings_after(&self, session_name: &str) -> Option<SavingsSample> {
        let text = must(
            self.rift,
            &["savings".to_owned(), "--json".to_owned()],
            &self.scratch,
        )
        .ok()?;
        let _ = std::fs::write(
            self.log_dir.join(format!("{session_name}.savings.json")),
            &text,
        );
        parse_savings(&text).ok()
    }

    /// The audit relation's rows by verb, at the end of one client's run.
    ///
    /// One reread report per client: it is the surface that groups the audit
    /// relation by verb, and it writes only to the scratch family's own
    /// projection.
    fn reread_kinds(&self) -> Vec<LeverKind> {
        capture(
            self.rift,
            &[
                "memory".to_owned(),
                "reread-report".to_owned(),
                "--json".to_owned(),
            ],
            &self.scratch,
        )
        .ok()
        .and_then(|(_, stdout, _)| {
            let _ = std::fs::write(self.log_dir.join("reread-report.json"), &stdout);
            parse_kinds(&stdout).ok()
        })
        .unwrap_or_default()
    }
}

/// What a run played: the readings, the calibrations, and every checkout that
/// has to be kept or swept afterwards.
struct Played {
    /// The runs the single-arm table and document draw — the on arm, when a
    /// client played two.
    readings: Vec<ClientRun>,
    /// The clients that played both arms.
    pairs: Vec<CompareRun>,
    /// The clients that played the shell arm beside their on arm.
    shells: Vec<ShellRun>,
    /// Checkouts that are neither: an arm whose twin never ran. It exists, so
    /// it is kept or swept with the rest — and it is deliberately **not** a
    /// reading, because half a calibration printed as one would be a share
    /// measured with the levers off.
    stray: Vec<ClientRun>,
    /// Shell-arm checkouts whose preparation failed part way. They are never
    /// read, but a clone and a registry row may already exist, so they are
    /// kept or swept with the rest.
    abandoned: Vec<PathBuf>,
}

impl Played {
    /// Every checkout this run made.
    fn checkouts(&self) -> impl Iterator<Item = &Path> {
        self.readings
            .iter()
            .chain(self.pairs.iter().map(|p| &p.off))
            .chain(self.shells.iter().map(|s| &s.shell))
            .chain(&self.stray)
            .map(|run| run.scratch.as_path())
            .chain(self.abandoned.iter().map(PathBuf::as_path))
    }

    /// File the arms one client played: a reading, a calibration and a shell
    /// reading, or a stray half of a set.
    fn absorb(&mut self, compare: bool, client: Client, played: Vec<ClientRun>) {
        match (compare, played.as_slice()) {
            (true, [off, on, shell @ ..]) => {
                self.readings.push(on.clone());
                self.pairs.push(CompareRun {
                    client,
                    off: off.clone(),
                    on: on.clone(),
                });
                if let [shell] = shell {
                    self.shells.push(ShellRun {
                        client,
                        on: on.clone(),
                        shell: shell.clone(),
                        transcripts: transcript_dir(&shell.scratch),
                    });
                }
            }
            // Half a calibration is not a reading; it is a checkout to sweep.
            (true, _) => self.stray.extend(played),
            (false, _) => self.readings.extend(played),
        }
    }
}

/// Play the scripted sessions and print the reading.
///
/// # Errors
/// A harness failure: running under CI, a missing session file, a scratch
/// checkout that resolved to the developer's own family, or a `rift` verb that
/// refused. A client that cannot run is recorded, not raised.
pub fn run(root: &Path, opts: &Options) -> Result<(), String> {
    refuse_under_ci()?;
    let all = load_sessions(&root.join(SESSIONS_DIR))?;
    let sessions: Vec<Session> = all
        .into_iter()
        .take(opts.sessions.unwrap_or(usize::MAX))
        .collect();
    let stamp = utc_stamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    );
    let out = output_dir(opts, &stamp)?;
    std::fs::create_dir_all(&out).map_err(|e| format!("dogfood: {}: {e}", out.display()))?;
    let rift = release_binary(root)?;
    let mine = read_savings(&rift, root)?;
    announce(opts, sessions.len(), &out, &mine);
    let untouched = codex_config().and_then(|p| Untouched::snapshot(p, &out));
    let played = play_clients(root, &rift, &out, &sessions, opts, &mine);
    if let Some(untouched) = &untouched {
        if untouched.restore() {
            println!(
                "dogfood: {} was changed by a client and has been put back as it was",
                untouched.path.display()
            );
        }
    }
    write_report(&out, &played.readings, &stamp, &opts.enforce)?;
    if opts.compare {
        let shells = if opts.bypass_shell {
            Some(played.shells.as_slice())
        } else {
            None
        };
        write_calibration(&out, &played.pairs, shells, &stamp)?;
    }
    println!("dogfood: logs   {}", out.display());
    // Both arms are kept, or both are swept: an arm left behind would be a
    // registry row and a family nobody meant to keep.
    if opts.keep {
        for scratch in played.checkouts() {
            println!(
                "dogfood: kept {} (its registry row is still there)",
                scratch.display()
            );
        }
    } else {
        for scratch in played.checkouts() {
            tidy(&rift, root, scratch);
        }
    }
    Ok(())
}

/// Refuse to start where nothing meant to start it.
///
/// # Errors
/// `CI` is set to anything non-empty. The command drives real clients and
/// spends real API quota, so it is never reachable from an automated run.
fn refuse_under_ci() -> Result<(), String> {
    if std::env::var("CI").is_ok_and(|v| !v.is_empty()) {
        return Err(
            "dogfood: this command drives real clients and spends real API quota; \
             it does not run under CI"
                .to_owned(),
        );
    }
    Ok(())
}

/// Where the scratch checkouts, the logs and the report go.
///
/// # Errors
/// No `--out` was given and there is no `HOME` to put the default under.
fn output_dir(opts: &Options, stamp: &str) -> Result<PathBuf, String> {
    let Some(dir) = &opts.out else {
        let home = std::env::var("HOME").map_err(|_| "dogfood: no HOME".to_owned())?;
        return Ok(PathBuf::from(home).join("rift-dogfood").join(stamp));
    };
    Ok(dir.clone())
}

/// Say what the run is about to spend, before it spends it.
fn announce(opts: &Options, sessions: usize, out: &Path, mine: &SavingsSample) {
    println!(
        "dogfood: {} client(s) x {} scripted session(s){} through the clients on this machine.\n\
         This SPENDS REAL API QUOTA on each one. Output: {}\n\
         The developer's own family ({}) is not touched.",
        opts.clients.len(),
        sessions,
        match (opts.compare, opts.bypass_shell) {
            (true, true) => " x 3 arms (levers off, then on, then on with shell reads only)",
            (true, false) => " x 2 arms (levers off, then on)",
            (false, _) => "",
        },
        out.display(),
        mine.recall_db
    );
    if !opts.enforce.is_empty() {
        println!(
            "dogfood: written at enforce in every arm but the off arm: {}",
            opts.enforce
                .iter()
                .map(|l| l.key())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

/// Play every client the run was asked for, one arm or two.
///
/// A client whose preparation failed is reported and skipped: one client that
/// cannot run is not a reason to lose the readings of the ones that can.
fn play_clients(
    root: &Path,
    rift: &Path,
    out: &Path,
    sessions: &[Session],
    opts: &Options,
    mine: &SavingsSample,
) -> Played {
    // The families this run is already measuring, the developer's first. Each
    // arm's checkout is refused if it resolves to one of them.
    let mut seen = vec![(mine.recall_db.clone(), mine.projection_db.clone())];
    let mut out_played = Played {
        readings: Vec::with_capacity(opts.clients.len()),
        pairs: Vec::new(),
        shells: Vec::new(),
        stray: Vec::new(),
        abandoned: Vec::new(),
    };
    for client in &opts.clients {
        if let (true, ShellArm::Skipped(why)) = (opts.bypass_shell, client.shell_arm()) {
            println!("dogfood: {why}");
        }
        let arms = arms_for(opts, *client);
        let mut played: Vec<ClientRun> = Vec::with_capacity(arms.len());
        for arm in arms {
            let of = ArmOf {
                client: *client,
                arm,
                enforce: &opts.enforce,
            };
            match run_client(root, rift, out, sessions, &seen, of) {
                Ok(run) => {
                    seen.push((run.recall_db.clone(), run.projection_db.clone()));
                    played.push(run);
                }
                Err(e) => {
                    println!(
                        "dogfood: {}{} could not be prepared — {e}",
                        client.slug(),
                        arm.map_or_else(String::new, Arm::heading)
                    );
                    abandon(&mut out_played, out, *client, arm);
                    break;
                }
            }
        }
        out_played.absorb(opts.compare, *client, played);
    }
    out_played
}

/// Keep a shell-arm checkout whose preparation failed on the sweep list.
///
/// Only the shell arm's: the two calibration arms keep the handling they have
/// always had, and the shell arm is the one whose half set must never be left
/// behind as a family nobody meant to keep.
fn abandon(played: &mut Played, out: &Path, client: Client, arm: Option<Arm>) {
    if arm != Some(Arm::Shell) {
        return;
    }
    let scratch = arm_dir(out, client, arm).join("repo");
    if scratch.exists() {
        played.abandoned.push(scratch);
    }
}

/// The directory one arm's checkout and logs sit under.
///
/// Without an arm the layout is what it has always been; each arm of a
/// calibration gets a checkout and a log directory of its own beneath it.
fn arm_dir(out: &Path, client: Client, arm: Option<Arm>) -> PathBuf {
    arm.map_or_else(
        || out.join(client.slug()),
        |arm| out.join(client.slug()).join(arm.label()),
    )
}

/// Where the client wrote a shell-arm checkout's transcripts, from `HOME`.
fn transcript_dir(scratch: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(claude_code_transcript_dir(Path::new(&home), scratch))
}

/// Play every scripted session for one client.
fn run_client(
    root: &Path,
    rift: &Path,
    out: &Path,
    sessions: &[Session],
    seen: &[(String, String)],
    of: ArmOf<'_>,
) -> Result<ClientRun, String> {
    let ArmOf { client, arm, .. } = of;
    let base = arm_dir(out, client, arm);
    let scratch = base.join("repo");
    let log_dir = base.join("logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("dogfood: {}: {e}", log_dir.display()))?;
    let prepared = prepare(root, rift, &scratch, seen, of)?;
    let playing = Playing {
        client,
        arm,
        program: resolve_program(client.program())?,
        rift,
        scratch,
        log_dir,
    };
    println!(
        "dogfood: {}{} — {} via {}, scratch {}",
        client.slug(),
        arm.map_or_else(String::new, Arm::heading),
        sessions.len(),
        playing.program.display(),
        playing.scratch.display()
    );
    if arm == Some(Arm::Shell) {
        if let Some(dir) = transcript_dir(&playing.scratch) {
            println!(
                "dogfood: each session's transcript lands in {}",
                dir.display()
            );
        }
    }
    let played = playing.play_all(sessions, prepared.baseline());
    // A run where no session captured a document falls back to the reading the
    // preparation ended on, which is the family as the sessions found it.
    let final_savings = played
        .iter()
        .rev()
        .find_map(|s| s.savings.clone())
        .or_else(|| Some(prepared.baseline().clone()));
    let kinds = playing.reread_kinds();
    Ok(ClientRun {
        client,
        scratch: playing.scratch,
        // The family this checkout minted, as the refusal above read it.
        recall_db: prepared.minted().recall_db.clone(),
        projection_db: prepared.minted().projection_db.clone(),
        sessions: played,
        final_savings,
        kinds,
    })
}

/// Print the table and write it, and the JSON document, beside the logs.
///
/// # Errors
/// Either file could not be written.
fn write_report(
    out: &Path,
    runs: &[ClientRun],
    stamp: &str,
    enforced: &[Lever],
) -> Result<(), String> {
    let table = render_table(runs);
    println!("\n{table}");
    let json_path = out.join("dogfood.json");
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(&report_json(runs, stamp, enforced)).unwrap_or_default(),
    )
    .map_err(|e| format!("dogfood: {}: {e}", json_path.display()))?;
    std::fs::write(out.join("dogfood.txt"), &table)
        .map_err(|e| format!("dogfood: writing the table: {e}"))?;
    println!("dogfood: report {}", json_path.display());
    Ok(())
}

/// Print the calibration and write it beside the single-arm report.
///
/// # Errors
/// The document or the table could not be written.
fn write_calibration(
    out: &Path,
    pairs: &[CompareRun],
    shells: Option<&[ShellRun]>,
    stamp: &str,
) -> Result<(), String> {
    let (calibration, document) = calibration(pairs, shells, stamp);
    println!("\n{calibration}");
    let path = out.join("compare.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&document).unwrap_or_default(),
    )
    .map_err(|e| format!("dogfood: {}: {e}", path.display()))?;
    std::fs::write(out.join("compare.txt"), &calibration)
        .map_err(|e| format!("dogfood: writing the calibration: {e}"))?;
    println!("dogfood: compare {}", path.display());
    Ok(())
}

/// What an exit code says about a session.
const fn status_for(code: Option<i32>) -> Status {
    match code {
        Some(0) => Status::Completed,
        // A time box fires as a signal, so the child has no code at all.
        None => Status::TimedOut,
        Some(_) => Status::Failed,
    }
}

/// The last thing a client said on stderr, capped so a stack trace cannot take
/// the table over.
fn last_stderr_line(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no stderr")
        .chars()
        .take(200)
        .collect()
}
