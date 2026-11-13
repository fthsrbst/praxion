//! The `praxion-hands` link against a fake helper.
//!
//! The fake is a shell script, so these tests run on the file system and are
//! Unix-only. It is written under `CARGO_TARGET_TMPDIR` (inside the crate's own
//! target directory), never into a system temporary directory.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use praxion_headless::{HandsLink, HandsReply, release_all};

/// A helper that speaks the protocol from `logs/orch1112/s3a-hands.md`.
const FAKE: &str = r#"#!/bin/sh
printf '%s\n' "$*" > "$0.args"
case "$1" in
  --release-all) exit 0 ;;
esac
printf 'READY\n'
while IFS= read -r line; do
  case "$line" in
    ARM) printf 'OK 0\n' ;;
    F*) printf 'OK %s\n' "$(printf '%s' "$line" | cut -d' ' -f2)" ;;
    REFUSE) printf 'REFUSED 3 stale\n' ;;
    IDLE) printf 'IDLE\n' ;;
    TAKE) printf 'TAKEOVER test_input\n' ;;
    BYE) printf 'BYE stdin_eof\n'; exit 0 ;;
    QUIT) exit 0 ;;
    *) printf 'REFUSED 0 malformed\n' ;;
  esac
done
exit 0
"#;

fn write_script(name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).expect("the target tmp dir");
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).expect("the fake helper");
    file.write_all(body.as_bytes()).expect("the fake body");
    let mut permissions = file.metadata().expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("executable");
    path
}

/// Poll until a matching reply arrives, or fail after five seconds.
fn wait_for(link: &mut HandsLink, want: impl Fn(&HandsReply) -> bool) -> HandsReply {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for reply in link.poll() {
            if want(&reply) {
                return reply;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the fake helper did not answer in time"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn spawn_waits_for_ready_arms_and_exchanges_lines() {
    let script = write_script("fake-serve.sh", FAKE);
    let mut link = HandsLink::spawn(&script, 1234, true).expect("the fake helper arms");

    // The command line the helper really got.
    let args = std::fs::read_to_string(format!("{}.args", script.display()))
        .expect("the helper wrote its arguments");
    assert_eq!(args.trim(), "--serve --pid 1234 --activate");

    // `spawn` sent ARM; the helper acknowledges it before any frame.
    assert_eq!(
        wait_for(&mut link, |reply| matches!(reply, HandsReply::Ok(_))),
        HandsReply::Ok(0)
    );

    link.send("F 7 1 5 10 -10 1 -").expect("a frame");
    assert_eq!(
        wait_for(&mut link, |reply| matches!(reply, HandsReply::Ok(_))),
        HandsReply::Ok(7)
    );

    link.send("REFUSE").expect("a refused frame");
    assert_eq!(
        wait_for(&mut link, |reply| matches!(
            reply,
            HandsReply::Refused(..)
        )),
        HandsReply::Refused(3, "stale".to_owned())
    );

    link.send("IDLE").expect("idle");
    assert_eq!(
        wait_for(&mut link, |reply| *reply == HandsReply::Idle),
        HandsReply::Idle
    );

    link.send("TAKE").expect("a takeover");
    assert_eq!(
        wait_for(&mut link, |reply| matches!(
            reply,
            HandsReply::Takeover(_)
        )),
        HandsReply::Takeover("test_input".to_owned())
    );

    // The helper says BYE and exits; that is one terminal reply, not two.
    link.send("BYE").expect("bye");
    assert_eq!(
        wait_for(&mut link, |reply| matches!(reply, HandsReply::Bye(_))),
        HandsReply::Bye("stdin_eof".to_owned())
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        link.poll().is_empty(),
        "a BYE is not followed by an Exited reply"
    );
}

#[test]
fn a_helper_that_exits_without_bye_reports_exited() {
    let script = write_script("fake-quit.sh", FAKE);
    let mut link = HandsLink::spawn(&script, 5, false).expect("the fake helper arms");
    link.send("QUIT").expect("quit");
    assert_eq!(
        wait_for(&mut link, |reply| *reply == HandsReply::Exited),
        HandsReply::Exited
    );
}

#[test]
fn a_helper_that_dies_before_ready_fails_the_spawn() {
    let script = write_script("fake-dead.sh", "#!/bin/sh\nexit 1\n");
    let error = HandsLink::spawn(&script, 5, false).expect_err("no READY");
    assert!(
        format!("{error}").contains("before READY"),
        "unexpected error: {error}"
    );

    let refusing = write_script("fake-refuse.sh", "#!/bin/sh\nprintf 'REFUSED 0 no_access\\n'\nexit 2\n");
    let error = HandsLink::spawn(&refusing, 5, false).expect_err("refused at start-up");
    assert!(format!("{error}").contains("no_access"), "{error}");
}

#[test]
fn release_all_runs_the_helper_once() {
    let script = write_script("fake-release.sh", FAKE);
    release_all(&script, 99).expect("release-all");
    let args = std::fs::read_to_string(format!("{}.args", script.display()))
        .expect("the helper wrote its arguments");
    assert_eq!(args.trim(), "--release-all --pid 99");
}
